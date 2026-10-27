//! `store` and `source`: stores near a property, their layouts, and store
//! products per food; plus the sourcing that `list check` and `list shop`
//! attach to missing entries (docs/SPEC.md, "Stores and sourcing"; the
//! engine is in [`crate::sourcing`]).

use serde_json::{json, Map, Value};

use crate::cli::{SourceCmd, SourceFlags, StoreCmd, StoreFlags};
use crate::context::Ctx;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::output::{to_record, Record, Report};
use crate::profiles::Profile;
use crate::sourcing::{self, Choice, Layout, Product, Shop, Store, Stores, Tier, Want};
use crate::util::now_iso;

fn clean(v: &Option<String>) -> Option<String> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Profiles whose `stores` name this store (`default` when it is the first).
fn used_by(ctx: &Ctx, id: &str) -> Vec<String> {
    crate::profiles::list(&ctx.resolved.config_path)
        .unwrap_or_default()
        .iter()
        .filter_map(|p| match p.stores.iter().position(|s| s == id) {
            Some(0) => Some(format!("{} (default)", p.name)),
            Some(_) => Some(p.name.clone()),
            None => None,
        })
        .collect()
}

/// Products sold at a store: its own and its chain's.
fn at_store<'a>(products: &'a [Product], stores: &Stores, s: &Store) -> Vec<&'a Product> {
    let chain = sourcing::chain_key(&s.chain);
    products
        .iter()
        .filter(|p| {
            p.store_id.as_deref() == Some(s.id.as_str())
                || (p.store_id.is_none()
                    && sourcing::product_chain(p, stores).is_some_and(|c| sourcing::chain_key(&c) == chain))
        })
        .collect()
}

fn store_record(ctx: &Ctx, db: &Db, s: &Store, products: usize) -> Result<Record> {
    let (layout, from) = sourcing::effective_layout(db, s)?;
    Ok(to_record(&json!({
        "id": s.id,
        "chain": s.chain,
        "name": s.name,
        "address": s.address,
        "website": s.website,
        "notes": s.notes,
        "layout": layout.to_text(),
        "layout_source": from,
        "products": products,
        "profiles": used_by(ctx, &s.id).join(", "),
    })))
}

fn apply_store(s: &mut Store, f: &StoreFlags) -> Result<()> {
    if let Some(c) = clean(&f.chain) {
        s.chain = c;
    }
    if let Some(n) = clean(&f.name) {
        s.name = n;
    }
    for (slot, v) in [(&mut s.address, &f.address), (&mut s.website, &f.website), (&mut s.notes, &f.notes)] {
        if v.is_some() {
            *slot = clean(v);
        }
    }
    if let Some(l) = &f.layout {
        s.layout = Some(Layout::parse(l)?.to_text());
    }
    Ok(())
}

pub fn store(ctx: &Ctx, cmd: StoreCmd) -> Result<()> {
    if let StoreCmd::Use { stores } = &cmd {
        return use_stores(ctx, stores);
    }
    let db = ctx.db()?;
    match cmd {
        StoreCmd::Add { id, f } => {
            let id = sourcing::validate_store_id(&id)?;
            let chain =
                clean(&f.chain).ok_or_else(|| AppError::usage("a store needs --chain (e.g. --chain Sprouts)"))?;
            let now = now_iso();
            let mut s = Store {
                id,
                name: chain.clone(),
                chain,
                created_at: now.clone(),
                updated_at: now,
                ..Default::default()
            };
            apply_store(&mut s, &f)?;
            sourcing::insert_store(&db, &s)?;
            ctx.info(&format!("added store {} ({}, chain {})", s.id, s.name, s.chain));
            ctx.emit_mutation(&Report::object("store", store_record(ctx, &db, &s, 0)?))
        }
        StoreCmd::Ls { chain } => {
            let products = sourcing::all_products(&db)?;
            let stores = sourcing::stores_by_id(&db)?;
            let want = chain.as_deref().map(sourcing::chain_key);
            let rows = sourcing::all_stores(&db)?
                .iter()
                .filter(|s| want.as_ref().is_none_or(|c| sourcing::chain_key(&s.chain) == *c))
                .map(|s| store_record(ctx, &db, s, at_store(&products, &stores, s).len()))
                .collect::<Result<Vec<_>>>()?;
            ctx.emit(&Report::list("stores", rows).table_columns(&[
                "id",
                "chain",
                "name",
                "address",
                "products",
                "layout_source",
                "profiles",
            ]))
        }
        StoreCmd::Show { store } => {
            let s = sourcing::resolve_store(&db, &store)?;
            let products = sourcing::all_products(&db)?;
            let stores = sourcing::stores_by_id(&db)?;
            let mine = at_store(&products, &stores, &s);
            let r = store_record(ctx, &db, &s, mine.len())?;
            let rows = mine.iter().map(|p| source_record(p, &stores)).collect();
            if ctx.out.format == crate::output::Format::Table {
                // the store, then its products
                ctx.emit(&Report::object("store", r))?;
                return ctx.emit(&Report::list("store_products", rows).table_columns(SOURCE_TABLE));
            }
            ctx.emit(&Report::object("store", r).meta("products", json!(rows)))
        }
        StoreCmd::Edit { store, f, clear } => {
            let mut s = sourcing::resolve_store(&db, &store)?;
            apply_store(&mut s, &f)?;
            for c in &clear {
                match c.trim().to_lowercase().as_str() {
                    "address" => s.address = None,
                    "website" => s.website = None,
                    "notes" => s.notes = None,
                    "layout" => s.layout = None,
                    other => {
                        return Err(AppError::usage(format!(
                            "cannot clear '{other}' (address, website, notes, layout)"
                        )))
                    }
                }
            }
            s.updated_at = now_iso();
            sourcing::update_store(&db, &s)?;
            ctx.info(&format!("updated store {}", s.id));
            ctx.emit_mutation(&Report::object("store", store_record(ctx, &db, &s, 0)?))
        }
        StoreCmd::Rm { store, force } => {
            let s = sourcing::resolve_store(&db, &store)?;
            let own =
                sourcing::all_products(&db)?.iter().filter(|p| p.store_id.as_deref() == Some(s.id.as_str())).count();
            if own > 0 && !force {
                return Err(AppError::invalid(format!(
                    "store {} has {own} product(s); remove them first or use --force",
                    s.id
                )));
            }
            db.transaction(|db| sourcing::delete_store(db, &s.id))?;
            let users = used_by(ctx, &s.id);
            if !users.is_empty() {
                ctx.warn(&format!("warning: still in the stores of profile(s) {} (see `store use`)", users.join(", ")));
            }
            ctx.info(&format!(
                "removed store {}{}",
                s.id,
                if own > 0 { format!(" and {own} product(s)") } else { String::new() }
            ));
            ctx.emit_mutation(&Report::object("removed", to_record(&json!({"store": s.id, "products": own}))))
        }
        StoreCmd::Layout { target, chain, layout, clear } => {
            let (store, chain) = if chain { (None, Some(target.as_str())) } else { (Some(target.as_str()), None) };
            layout_cmd(ctx, &db, store, chain, layout.as_deref(), clear)
        }
        StoreCmd::Use { .. } => unreachable!("handled above"),
    }
}

fn layout_cmd(
    ctx: &Ctx,
    db: &Db,
    store: Option<&str>,
    chain: Option<&str>,
    layout: Option<&str>,
    clear: bool,
) -> Result<()> {
    let new = layout.map(Layout::parse).transpose()?;
    let (l, from, who) = match (store, chain) {
        (_, Some(c)) => {
            if new.is_some() || clear {
                sourcing::set_chain_layout(db, c, new.as_ref())?;
                ctx.info(&format!("{} the layout of chain {}", if clear { "removed" } else { "set" }, c.trim()));
            }
            match sourcing::chain_layout(db, c)? {
                Some(t) => (Layout::parse(&t)?, "chain", json!({"chain": c.trim()})),
                None => (Layout::default_layout(), "default", json!({"chain": c.trim()})),
            }
        }
        (Some(s), None) => {
            let mut st = sourcing::resolve_store(db, s)?;
            if new.is_some() || clear {
                st.layout = new.as_ref().map(Layout::to_text);
                st.updated_at = now_iso();
                sourcing::update_store(db, &st)?;
                ctx.info(&format!("{} the layout of store {}", if clear { "removed" } else { "set" }, st.id));
            }
            let (l, from) = sourcing::effective_layout(db, &st)?;
            (l, from, json!({"store": st.id, "chain": st.chain}))
        }
        (None, None) => return Err(AppError::usage("give a store or --chain")),
    };
    let rows = l
        .0
        .iter()
        .enumerate()
        .map(|(i, s)| {
            to_record(&json!({"n": i + 1, "section": s.section, "title": sourcing::section_title(&s.section), "aisles": s.aisles}))
        })
        .collect();
    let mut r = Report::list("store_layout", rows)
        .table_columns(&["n", "section", "title", "aisles"])
        .meta("layout", json!(l.to_text()))
        .meta("layout_source", json!(from));
    if let Value::Object(m) = who {
        for (k, v) in m {
            r = r.meta(&k, v);
        }
    }
    ctx.emit(&r)
}

fn use_stores(ctx: &Ctx, specs: &[String]) -> Result<()> {
    let p =
        ctx.profile.clone().ok_or_else(|| AppError::usage("`store use` sets a profile's stores: give -P PROFILE"))?;
    let db = ctx.db()?;
    let mut ids: Vec<String> = Vec::new();
    for s in specs {
        let id = sourcing::resolve_store(&db, s)?.id;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    crate::profiles::write_stores(&ctx.resolved.config_path, &p.name, &ids)?;
    ctx.info(&if ids.is_empty() {
        format!("profile {} has no stores", p.name)
    } else {
        format!("profile {} shops at {} (default {})", p.name, ids.join(", "), ids[0])
    });
    ctx.emit_mutation(&Report::object("profile_stores", to_record(&json!({"profile": p.name, "stores": ids}))))
}

// ---------------------------------------------------------------------------
// source
// ---------------------------------------------------------------------------

/// Apply `source add`/`edit` flags; returns the section guess, if one was made.
fn apply_source(ctx: &Ctx, db: &Db, p: &mut Product, f: &SourceFlags, guess: bool) -> Result<Option<sourcing::Guess>> {
    if let Some(s) = clean(&f.store) {
        p.store_id = Some(sourcing::resolve_store(db, &s)?.id);
        p.chain = None;
    }
    if let Some(c) = clean(&f.chain) {
        p.chain = Some(c);
        p.store_id = None;
    }
    if let Some(n) = clean(&f.name) {
        p.name = n;
    }
    for (slot, v) in [
        (&mut p.brand, &f.brand),
        (&mut p.size, &f.size),
        (&mut p.url, &f.url),
        (&mut p.sku, &f.sku),
        (&mut p.plu, &f.plu),
        (&mut p.upc, &f.upc),
        (&mut p.aisle, &f.aisle),
        (&mut p.notes, &f.note),
    ] {
        if v.is_some() {
            *slot = clean(v);
        }
    }
    let fmts = ctx.input_date_formats();
    let fmts: Vec<&str> = fmts.iter().map(String::as_str).collect();
    if let Some(price) = clean(&f.price) {
        p.price = Some(crate::util::parse_money(&price)?);
        if f.seen.is_none() {
            p.price_seen = Some(ctx.tz.today().format("%Y-%m-%d").to_string());
        }
    }
    if let Some(d) = &f.seen {
        p.price_seen = clean(&Some(d.clone())).map(|d| crate::util::parse_day(&d, &fmts, &ctx.tz)).transpose()?;
    }
    if let Some(i) = clean(&f.item) {
        p.item_id = Some(crate::store::resolve_item(db, &i)?.id);
    }
    if let Some(s) = clean(&f.section) {
        let (id, custom) = sourcing::section_id(&s).expect("not empty");
        if custom {
            ctx.info(&format!("custom section '{id}' (not one of: {})", section_names()));
        }
        p.section = Some(id);
        return Ok(None);
    }
    if guess && p.section.is_none() {
        let g = sourcing::guess_section(f.category.as_deref(), p.url.as_deref(), Some(&p.name), Some(&p.food));
        p.section = g.as_ref().map(|g| g.section.clone());
        return Ok(g);
    }
    Ok(None)
}

fn section_names() -> String {
    sourcing::SECTIONS.iter().map(|s| s.0).collect::<Vec<_>>().join(", ")
}

fn source_record(p: &Product, stores: &Stores) -> Record {
    let mut r = to_record(&sourcing::product_json(p, stores));
    r.insert("at".into(), json!(sourcing::product_place(p, stores)));
    r
}

const SOURCE_TABLE: &[&str] = &["id", "food", "at", "name", "size", "price", "section", "aisle", "notes"];

pub fn source(ctx: &Ctx, cmd: SourceCmd) -> Result<()> {
    let db = ctx.db()?;
    match cmd {
        SourceCmd::Add { food, f } => {
            let food = food.trim().to_string();
            if food.is_empty() {
                return Err(AppError::usage("give the food this product is for"));
            }
            if f.store.is_none() && f.chain.is_none() {
                return Err(AppError::usage("give --store S (one store) or --chain C (every store of a chain)"));
            }
            let now = now_iso();
            let mut p = Product {
                food_key: sourcing::food_key(&food),
                name: food.clone(),
                food,
                created_at: now.clone(),
                updated_at: now,
                ..Default::default()
            };
            let guess = apply_source(ctx, &db, &mut p, &f, true)?;
            p.id = sourcing::insert_product(&db, &p)?;
            let stores = sourcing::stores_by_id(&db)?;
            let section = match (&p.section, &guess) {
                (Some(s), Some(g)) => format!(", section {s} (guessed from the {})", g.basis),
                (Some(s), None) => format!(", section {s}"),
                (None, _) => ", no section (give --section)".to_string(),
            };
            ctx.info(&format!(
                "added store product {}: {} at {} for {}{section}",
                p.id,
                p.name,
                sourcing::product_place(&p, &stores),
                p.food
            ));
            let mut r = source_record(&p, &stores);
            r.insert("section_guessed".into(), json!(guess.is_some()));
            r.insert("section_basis".into(), json!(guess.map(|g| g.basis)));
            ctx.emit_mutation(&Report::object("store_product", r))
        }
        SourceCmd::Ls { food, store, chain } => {
            let stores = sourcing::stores_by_id(&db)?;
            let products = sourcing::all_products(&db)?;
            let at = store.as_deref().map(|s| sourcing::resolve_store(&db, s)).transpose()?;
            let want = food.as_ref().map(|f| Want { names: vec![f.clone()], items: Vec::new() });
            let chain = chain.as_deref().map(sourcing::chain_key);
            let mut rows: Vec<(Option<sourcing::Link>, Record)> = products
                .iter()
                .filter(|p| at.as_ref().is_none_or(|s| at_store(std::slice::from_ref(p), &stores, s).len() == 1))
                .filter(|p| {
                    chain.as_ref().is_none_or(|c| {
                        sourcing::product_chain(p, &stores).is_some_and(|x| sourcing::chain_key(&x) == *c)
                    })
                })
                .filter_map(|p| match &want {
                    Some(w) => sourcing::link(p, w).map(|l| (Some(l), p)),
                    None => Some((None, p)),
                })
                .map(|(l, p)| {
                    let mut r = source_record(p, &stores);
                    if let Some(l) = l {
                        r.insert("link".into(), json!(l.as_str()));
                    }
                    (l, r)
                })
                .collect();
            rows.sort_by_key(|(l, _)| *l);
            let mut cols = SOURCE_TABLE.to_vec();
            if want.is_some() {
                cols.push("link");
            }
            ctx.emit(&Report::list("store_products", rows.into_iter().map(|(_, r)| r).collect()).table_columns(&cols))
        }
        SourceCmd::Edit { id, food, f, clear } => {
            let mut p = sourcing::resolve_product(&db, &id)?;
            if let Some(fd) = clean(&food) {
                p.food_key = sourcing::food_key(&fd);
                p.food = fd;
            }
            apply_source(ctx, &db, &mut p, &f, false)?;
            for c in &clear {
                match c.trim().to_lowercase().as_str() {
                    "brand" => p.brand = None,
                    "size" => p.size = None,
                    "price" => (p.price, p.price_seen) = (None, None),
                    "url" => p.url = None,
                    "sku" => p.sku = None,
                    "plu" => p.plu = None,
                    "upc" => p.upc = None,
                    "section" => p.section = None,
                    "aisle" => p.aisle = None,
                    "note" | "notes" => p.notes = None,
                    "item" => p.item_id = None,
                    other => {
                        return Err(AppError::usage(format!(
                        "cannot clear '{other}' (brand, size, price, url, sku, plu, upc, section, aisle, note, item)"
                    )))
                    }
                }
            }
            p.updated_at = now_iso();
            sourcing::update_product(&db, &p)?;
            ctx.info(&format!("updated store product {}", p.id));
            ctx.emit_mutation(&Report::object("store_product", source_record(&p, &sourcing::stores_by_id(&db)?)))
        }
        SourceCmd::Rm { ids } => {
            let ps = ids.iter().map(|i| sourcing::resolve_product(&db, i)).collect::<Result<Vec<_>>>()?;
            db.transaction(|db| ps.iter().try_for_each(|p| sourcing::delete_product(db, p.id)))?;
            ctx.info(&format!("removed {} store product(s)", ps.len()));
            ctx.emit_mutation(&Report::object(
                "removed",
                to_record(&json!({"store_products": ps.iter().map(|p| p.id).collect::<Vec<_>>()})),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Sourcing for list check / list shop
// ---------------------------------------------------------------------------

/// The stores and products of one database, and the store in use.
pub struct Sourcing {
    pub shop: Shop,
    pub stores: Stores,
    pub products: Vec<Product>,
    pub layout: Layout,
    pub layout_source: &'static str,
}

impl Sourcing {
    /// The store in use: `--store`, else the profile's first store in the
    /// database (configured ids missing from it are warned about).
    pub fn load(ctx: &Ctx, p: Option<&Profile>, db: &Db, flag: Option<&str>) -> Result<Self> {
        let stores = sourcing::stores_by_id(db)?;
        let products = sourcing::all_products(db)?;
        let configured: Vec<String> = p.or(ctx.profile.as_ref()).map(|p| p.stores.clone()).unwrap_or_default();
        let missing: Vec<&String> = configured.iter().filter(|s| !stores.contains_key(*s)).collect();
        if !missing.is_empty() {
            let who = p.or(ctx.profile.as_ref()).map(|p| p.name.clone()).unwrap_or_default();
            ctx.warn(&format!(
                "warning: profile {who} names store(s) not in its database: {}",
                missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        let known: Vec<String> = configured.into_iter().filter(|s| stores.contains_key(s)).collect();
        let store = match flag {
            Some(f) => Some(sourcing::resolve_store(db, f)?),
            None => known.first().and_then(|s| stores.get(s)).cloned(),
        };
        let others = known.into_iter().filter(|s| store.as_ref().is_none_or(|st| st.id != *s)).collect();
        let (layout, layout_source) = match &store {
            Some(s) => sourcing::effective_layout(db, s)?,
            None => (Layout::default_layout(), "default"),
        };
        Ok(Self { shop: Shop { store, others }, stores, products, layout, layout_source })
    }

    /// Whether there is anything to say about sourcing (a store, or products).
    pub fn active(&self) -> bool {
        self.shop.store.is_some() || !self.products.is_empty()
    }

    pub fn choose(&self, w: &Want) -> Option<Choice> {
        sourcing::choose(&self.products, &self.stores, &self.shop, w)
    }

    /// The store-in-use fields of a row.
    pub fn store_fields(&self) -> Map<String, Value> {
        let s = self.shop.store.as_ref();
        let mut m = Map::new();
        m.insert("store".into(), json!(s.map(|s| s.id.clone())));
        m.insert("store_name".into(), json!(s.map(|s| s.name.clone())));
        m
    }

    /// The sourcing fields of an entry that needs buying (`need` of `unit`).
    pub fn fields(&self, c: Option<&Choice>, need: f64, unit: &str) -> Map<String, Value> {
        let mut m = self.store_fields();
        let p = c.map(|c| &c.product);
        let aisle = p.and_then(|p| {
            p.aisle.clone().or_else(|| p.section.as_deref().and_then(|s| self.layout.aisles(s)).map(str::to_string))
        });
        m.insert("source_tier".into(), json!(c.map(|c| c.tier.as_str())));
        m.insert("source_link".into(), json!(c.map(|c| c.link.as_str())));
        m.insert("source_id".into(), json!(p.map(|p| p.id)));
        m.insert("source_at".into(), json!(p.map(|p| sourcing::product_place(p, &self.stores))));
        m.insert("product".into(), json!(p.map(|p| p.name.clone())));
        m.insert("brand".into(), json!(p.and_then(|p| p.brand.clone())));
        m.insert("size".into(), json!(p.and_then(|p| p.size.clone())));
        m.insert("price".into(), json!(p.and_then(|p| p.price).map(crate::util::money_value)));
        m.insert("url".into(), json!(p.and_then(|p| p.url.clone())));
        m.insert("section".into(), json!(p.and_then(|p| p.section.clone())));
        m.insert("aisle".into(), json!(aisle));
        m.insert("source_note".into(), json!(p.and_then(|p| p.notes.clone())));
        m.insert("packs".into(), json!(p.and_then(|p| sourcing::packs(need, unit, p))));
        m.insert("buy".into(), json!(self.buy_text(c, aisle.as_deref())));
        m
    }

    /// `Sprouts North: Raw Walnut Halves (bulk, aisle 2; top bins)`, or
    /// `no source at Sprouts North`.
    pub fn buy_text(&self, c: Option<&Choice>, aisle: Option<&str>) -> String {
        let store = self.shop.store.as_ref();
        let Some(c) = c else {
            return store.map_or_else(|| "no source".to_string(), |s| format!("no source at {}", s.name));
        };
        let p = &c.product;
        let place = sourcing::product_place(p, &self.stores);
        let place = match (c.tier, store) {
            (Tier::Other, Some(s)) => format!("{place} (none at {})", s.name),
            (Tier::Store, Some(s)) => s.name.clone(),
            _ => place,
        };
        let mut where_: Vec<String> = Vec::new();
        if let Some(s) = &p.section {
            where_.push(s.clone());
        }
        if let Some(a) = aisle {
            where_.push(format!("aisle {a}"));
        }
        let mut detail = where_.join(", ");
        if let Some(n) = &p.notes {
            detail = if detail.is_empty() { n.clone() } else { format!("{detail}; {n}") };
        }
        if detail.is_empty() {
            format!("{place}: {}", p.name)
        } else {
            format!("{place}: {} ({detail})", p.name)
        }
    }
}
