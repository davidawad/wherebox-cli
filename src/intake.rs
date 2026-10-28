//! Receipt intake (docs/SPEC.md, "Receipts"): matching parsed receipt lines
//! to items, the review file, applying accepted lines as lots and store
//! product prices, and the stored receipts.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};

use crate::db::{int, opt_int, opt_text, text, Db, Row, RowExt};
use crate::error::{AppError, Result};
use crate::food::Lexicon;
use crate::matching::{self, Dim};
use crate::receipt::{self, dollars, dollars_opt, Line, Receipt, Skipped};
use crate::shelf_life::Types;
use crate::sourcing::{self, Product, Store, Stores};
use crate::store::{self, Item, Lot, Source, Tree};
use crate::util::now_iso;

/// The review file's schema.
pub const REVIEW_SCHEMA: &str = "wherebox/receipt-review/v1";

// ---------------------------------------------------------------------------
// Stored receipts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredReceipt {
    pub id: String,
    pub fingerprint: String,
    pub store_id: Option<String>,
    pub chain: Option<String>,
    pub purchased_on: Option<String>,
    pub purchased_time: Option<String>,
    #[serde(with = "dollars_opt", default)]
    pub subtotal: Option<i64>,
    #[serde(with = "dollars_opt", default)]
    pub tax: Option<i64>,
    #[serde(with = "dollars_opt", default)]
    pub total: Option<i64>,
    /// Lines imported.
    #[serde(default)]
    pub lines: i64,
    pub ocr_text: String,
    pub source_path: Option<String>,
    pub image_name: Option<String>,
    /// The original file, base64 (only with `--keep-image`).
    pub image: Option<String>,
    pub created_at: String,
}

const RECEIPT_COLS: &str = "id, fingerprint, store_id, chain, purchased_on, purchased_time, subtotal, tax, total, \
    lines, ocr_text, source_path, image_name, image, created_at";

fn receipt_from(r: &Row) -> StoredReceipt {
    StoredReceipt {
        id: r.s(0).unwrap_or_default(),
        fingerprint: r.s(1).unwrap_or_default(),
        store_id: r.s(2),
        chain: r.s(3),
        purchased_on: r.s(4),
        purchased_time: r.s(5),
        subtotal: r.i(6),
        tax: r.i(7),
        total: r.i(8),
        lines: r.i(9).unwrap_or_default(),
        ocr_text: r.s(10).unwrap_or_default(),
        source_path: r.s(11),
        image_name: r.s(12),
        image: r.s(13),
        created_at: r.s(14).unwrap_or_default(),
    }
}

fn receipt_params(r: &StoredReceipt) -> Vec<crate::db::Value> {
    vec![
        text(&r.id),
        text(&r.fingerprint),
        opt_text(r.store_id.as_ref()),
        opt_text(r.chain.as_ref()),
        opt_text(r.purchased_on.as_ref()),
        opt_text(r.purchased_time.as_ref()),
        opt_int(r.subtotal),
        opt_int(r.tax),
        opt_int(r.total),
        int(r.lines),
        text(&r.ocr_text),
        opt_text(r.source_path.as_ref()),
        opt_text(r.image_name.as_ref()),
        opt_text(r.image.as_ref()),
        text(&r.created_at),
    ]
}

pub fn insert_receipt(db: &Db, r: &StoredReceipt) -> Result<()> {
    let ph = (1..=15).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
    db.execute(&format!("INSERT INTO receipts ({RECEIPT_COLS}) VALUES ({ph})"), &receipt_params(r)).map(|_| ())
}

fn update_receipt(db: &Db, r: &StoredReceipt) -> Result<()> {
    let sets = RECEIPT_COLS
        .split(", ")
        .enumerate()
        .skip(1)
        .map(|(n, c)| format!("{c} = ?{}", n + 1))
        .collect::<Vec<_>>()
        .join(", ");
    db.execute(&format!("UPDATE receipts SET {sets} WHERE id = ?1"), &receipt_params(r)).map(|_| ())
}

pub fn get_receipt(db: &Db, id: &str) -> Result<Option<StoredReceipt>> {
    Ok(db
        .query_opt(&format!("SELECT {RECEIPT_COLS} FROM receipts WHERE id = ?1"), &[text(id)])?
        .map(|r| receipt_from(&r)))
}

/// Every receipt, newest purchase first.
pub fn all_receipts(db: &Db) -> Result<Vec<StoredReceipt>> {
    let mut v: Vec<StoredReceipt> =
        db.query(&format!("SELECT {RECEIPT_COLS} FROM receipts"), &[])?.iter().map(receipt_from).collect();
    v.sort_by(|a, b| {
        (&b.purchased_on, &b.purchased_time, &b.created_at).cmp(&(&a.purchased_on, &a.purchased_time, &a.created_at))
    });
    Ok(v)
}

pub fn by_fingerprint(db: &Db, fp: &str) -> Result<Vec<StoredReceipt>> {
    Ok(db
        .query(&format!("SELECT {RECEIPT_COLS} FROM receipts WHERE fingerprint = ?1"), &[text(fp)])?
        .iter()
        .map(receipt_from)
        .collect())
}

/// A receipt by id, or a unique id prefix or suffix (6+ characters).
pub fn resolve_receipt(db: &Db, spec: &str) -> Result<StoredReceipt> {
    let up = spec.trim().to_ascii_uppercase();
    if let Some(r) = get_receipt(db, &up)? {
        return Ok(r);
    }
    let hits: Vec<StoredReceipt> = if up.len() >= 6 {
        all_receipts(db)?.into_iter().filter(|r| r.id.starts_with(&up) || r.id.ends_with(&up)).collect()
    } else {
        vec![]
    };
    match hits.len() {
        1 => Ok(hits.into_iter().next().expect("one")),
        0 => Err(AppError::not_found(format!("no receipt '{spec}' (see `wherebox receipt ls`)"))),
        n => Err(AppError::invalid(format!("'{spec}' matches {n} receipts; give more of the id"))),
    }
}

/// The `source_ref` prefix of a receipt's lots.
pub fn lot_prefix(id: &str) -> String {
    format!("receipt:{id}:")
}

/// A receipt's lots, by line.
pub fn receipt_lots(db: &Db, id: &str) -> Result<Vec<Lot>> {
    let prefix = lot_prefix(id);
    let line = |l: &Lot| {
        l.source_ref.as_deref().and_then(|r| r.strip_prefix(&prefix)).and_then(|n| n.parse::<usize>().ok()).unwrap_or(0)
    };
    let mut v: Vec<Lot> = store::all_lots(db)?
        .into_iter()
        .filter(|l| l.source_ref.as_deref().is_some_and(|r| r.starts_with(&prefix)))
        .collect();
    v.sort_by_key(|l| line(l));
    Ok(v)
}

/// The envelope field of an export: every receipt.
pub fn export(db: &Db) -> Result<Json> {
    Ok(json!(all_receipts(db)?))
}

/// Create or update the receipts of an envelope (by id); returns how many.
pub fn import(db: &Db, envelope: &Json) -> Result<usize> {
    let mut n = 0;
    for v in envelope.get("receipts").and_then(Json::as_array).into_iter().flatten() {
        let r: StoredReceipt =
            serde_json::from_value(v.clone()).map_err(|e| AppError::invalid(format!("a receipt: {e}")))?;
        if r.id.trim().is_empty() || r.fingerprint.trim().is_empty() {
            return Err(AppError::invalid("a receipt without an id or fingerprint"));
        }
        match get_receipt(db, &r.id)? {
            Some(_) => update_receipt(db, &r)?,
            None => insert_receipt(db, &r)?,
        }
        n += 1;
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// The store of a receipt
// ---------------------------------------------------------------------------

/// The store a receipt was printed by, and notes on how it was chosen.
#[derive(Debug, Clone, Default)]
pub struct StoreGuess {
    pub store: Option<Store>,
    /// The chain (the store's, else the detected one).
    pub chain: Option<String>,
    /// Several stores fit; a pick is needed.
    pub candidates: Vec<Store>,
    pub note: Option<String>,
}

fn address_in(header: &[String], addr: &str) -> bool {
    let first = addr.split([',', '\n']).next().unwrap_or_default();
    let want = receipt::words(first);
    let have: Vec<String> = header.iter().flat_map(|h| receipt::words(h)).collect();
    !want.is_empty() && have.windows(want.len()).any(|w| w == want.as_slice())
}

/// Find the store: the one store of the chain, else the one whose address
/// is in the header, else the first of the profile's stores among them.
pub fn guess_store(db: &Db, r: &Receipt, profile_stores: &[String]) -> Result<StoreGuess> {
    let stores = sourcing::all_stores(db)?;
    let header_words: Vec<String> = r.header.iter().flat_map(|h| receipt::words(h)).collect();
    let named = |s: &str| {
        let w = receipt::words(s);
        !w.is_empty() && header_words.windows(w.len()).any(|x| x == w.as_slice())
    };
    let cands: Vec<Store> = match &r.chain {
        Some(c) => stores.into_iter().filter(|s| sourcing::chain_key(&s.chain) == sourcing::chain_key(c)).collect(),
        None => stores.into_iter().filter(|s| named(&s.chain) || named(&s.name)).collect(),
    };
    let chain = r.chain.clone().or_else(|| cands.first().map(|s| s.chain.clone()));
    let one = |s: Store| Ok(StoreGuess { chain: Some(s.chain.clone()), store: Some(s), ..Default::default() });
    match cands.len() {
        0 => Ok(StoreGuess {
            note: chain.as_ref().map(|c| {
                format!("no {c} store in this database: lots get the chain as vendor (add one with `wherebox store add ID --chain {c:?} --address ...`)")
            }),
            chain,
            ..Default::default()
        }),
        1 => one(cands.into_iter().next().expect("one")),
        _ => {
            let by_addr: Vec<&Store> =
                cands.iter().filter(|s| s.address.as_deref().is_some_and(|a| address_in(&r.header, a))).collect();
            if by_addr.len() == 1 {
                return one(by_addr[0].clone());
            }
            if let Some(s) = profile_stores.iter().find_map(|id| cands.iter().find(|s| &s.id == id)) {
                return one(s.clone());
            }
            Ok(StoreGuess {
                note: Some(format!(
                    "{} stores of {} fit ({}); use --store",
                    cands.len(),
                    chain.as_deref().unwrap_or("the chain"),
                    cands.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", ")
                )),
                chain,
                candidates: cands,
                ..Default::default()
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// What a line should become.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// `add` (to an existing item) or `new`.
    pub action: &'static str,
    pub item: Option<(String, String)>,
    pub new_name: Option<String>,
    pub product: Option<i64>,
    /// `code`, `product`, `food`, `item`, `new`.
    pub tier: &'static str,
    pub confidence: f64,
    pub shelf_life: Option<String>,
}

/// Matches receipt lines against a database's store products and items.
pub struct Matcher {
    items: Vec<Item>,
    with_lots: HashSet<String>,
    products: Vec<Product>,
    stores: Stores,
    store: Option<String>,
    chain: Option<String>,
    lex: Lexicon,
    types: Types,
    threshold: f64,
}

fn round4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

/// Does a product carry this code (PLU, SKU, or a UPC compared as a GTIN)?
pub fn code_matches(p: &Product, code: &str) -> bool {
    let id = matching::normalize_id(code);
    let gtin = matching::normalize_gtin(code);
    let eq = |v: &Option<String>| v.as_deref().is_some_and(|v| matching::normalize_id(v) == id);
    eq(&p.plu)
        || eq(&p.sku)
        || p.upc
            .as_deref()
            .is_some_and(|u| matching::normalize_id(u) == id || (gtin.is_some() && matching::normalize_gtin(u) == gtin))
}

impl Matcher {
    pub fn load(db: &Db, types: &Types, store: Option<&Store>, chain: Option<&str>, threshold: f64) -> Result<Self> {
        Ok(Self {
            items: store::all_items(db)?.into_iter().filter(|i| !i.is_gone()).collect(),
            with_lots: store::all_lots(db)?.into_iter().map(|l| l.item_id).collect(),
            products: sourcing::all_products(db)?,
            stores: sourcing::stores_by_id(db)?,
            store: store.map(|s| s.id.clone()),
            chain: store.map(|s| s.chain.as_str()).or(chain).map(sourcing::chain_key),
            lex: Lexicon::new(types),
            types: types.clone(),
            threshold,
        })
    }

    /// 0: the store's own product, 1: its chain's, 2: any other.
    fn scope(&self, p: &Product) -> u8 {
        if self.store.is_some() && p.store_id == self.store {
            return 0;
        }
        let chain = sourcing::product_chain(p, &self.stores).map(|c| sourcing::chain_key(&c));
        if self.chain.is_some() && chain == self.chain {
            1
        } else {
            2
        }
    }

    /// The item a store product stands for: its linked item, else the item
    /// named like its food.
    fn product_item(&self, p: &Product) -> Option<&Item> {
        if let Some(i) = p.item_id.as_ref().and_then(|id| self.items.iter().find(|i| &i.id == id)) {
            return Some(i);
        }
        if let Some(i) = self.items.iter().find(|i| sourcing::food_key(&i.name) == p.food_key) {
            return Some(i);
        }
        self.items
            .iter()
            .map(|i| (i, matching::score_names(&p.food, &i.name).score))
            .filter(|(_, s)| *s >= self.threshold)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    fn via_product(&self, p: &Product, tier: &'static str, confidence: f64) -> Proposal {
        let base = Proposal {
            action: "new",
            item: None,
            new_name: None,
            product: Some(p.id),
            tier,
            confidence: round4(confidence),
            shelf_life: None,
        };
        match self.product_item(p) {
            Some(i) => Proposal { action: "add", item: Some((i.id.clone(), i.name.clone())), ..base },
            None => {
                let name = title_words(&p.food);
                Proposal { shelf_life: self.shelf_life(&name), new_name: Some(name), ..base }
            }
        }
    }

    fn names_food(&self, name: &str) -> bool {
        self.lex.names_food(name) || self.types.by_name(name).is_some()
    }

    /// Receipts often print the noun first (`AVOCADO HASS`, `ONION
    /// YELLOW`): when the head noun is not a food but the first word is,
    /// read it last (`Hass Avocado`).
    pub fn food_order(&self, name: &str) -> String {
        let ws: Vec<&str> = name.split_whitespace().collect();
        if ws.len() < 2 || self.names_food(name) || !self.names_food(ws[0]) {
            return name.to_string();
        }
        format!("{} {}", ws[1..].join(" "), ws[0])
    }

    fn shelf_life(&self, name: &str) -> Option<String> {
        self.types.by_name(name).map(|(t, _)| t.name.clone())
    }

    /// The proposal for a line (see the spec's ladder).
    pub fn propose(&self, l: &Line) -> Proposal {
        let name = if l.name.trim().is_empty() { receipt::expand(&l.text, receipt::generic()) } else { l.name.clone() };
        let name = self.food_order(&name);
        // 1. code
        if let Some(code) = &l.code {
            if let Some(p) =
                self.products.iter().filter(|p| code_matches(p, code)).min_by_key(|p| (self.scope(p), p.id))
            {
                return self.via_product(p, "code", 1.0);
            }
        }
        // 2. store product name
        let near = self.store.is_some() || self.chain.is_some();
        let best = self
            .products
            .iter()
            .filter(|p| !near || self.scope(p) < 2)
            .map(|p| {
                let s = matching::score_names(&name, &p.name).score.max(matching::score_names(&name, &p.food).score);
                (p, s)
            })
            .filter(|(_, s)| *s >= self.threshold)
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.id.cmp(&a.0.id)));
        if let Some((p, s)) = best {
            return self.via_product(p, "product", 0.95 * s);
        }
        let add = |i: &Item, tier, c: f64| Proposal {
            action: "add",
            item: Some((i.id.clone(), i.name.clone())),
            new_name: None,
            product: None,
            tier,
            confidence: round4(c),
            shelf_life: None,
        };
        // 3. food: the head noun names a food, and a consumable has it as its head
        if self.names_food(&name) {
            let segs = matching::segments(&name);
            let toks = matching::name_tokens(&name);
            let head = |i: &Item| {
                let a = matching::head_match(&matching::name_tokens(&i.name), &segs);
                let b = matching::head_match(&toks, &matching::segments(&i.name));
                [a, b].iter().filter(|h| h.score >= 0.85 && !h.compound).map(|h| h.score).fold(0.0, f64::max)
            };
            let best = self
                .items
                .iter()
                .filter(|i| self.with_lots.contains(&i.id) || self.lex.names_food(&i.name))
                .map(|i| (i, head(i)))
                .filter(|(_, s)| *s > 0.0)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((i, s)) = best {
                return add(i, "food", 0.9 * s);
            }
        }
        // 4. any item by fuzzy name
        let best = self
            .items
            .iter()
            .map(|i| (i, matching::score_names(&name, &i.name).score))
            .filter(|(_, s)| *s >= self.threshold)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, s)) = best {
            return add(i, "item", 0.9 * s);
        }
        // 5. a new item
        let shelf_life = self.shelf_life(&name);
        let food = shelf_life.is_some() || self.names_food(&name);
        Proposal {
            action: "new",
            item: None,
            new_name: Some(name),
            product: None,
            tier: "new",
            confidence: if food { 0.8 } else { 0.4 },
            shelf_life,
        }
    }
}

/// `walnut halves` -> `Walnut Halves`.
pub fn title_words(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// The review
// ---------------------------------------------------------------------------

/// The receipt part of a review.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Head {
    pub fingerprint: String,
    /// The file as given (null with `--no-path`).
    #[serde(default)]
    pub file: Option<String>,
    /// `text`, `pdf`, `image`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub chain: Option<String>,
    #[serde(default)]
    pub store: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub time: Option<String>,
    #[serde(with = "dollars_opt", default)]
    pub subtotal: Option<i64>,
    #[serde(with = "dollars_opt", default)]
    pub tax: Option<i64>,
    #[serde(with = "dollars_opt", default)]
    pub total: Option<i64>,
    /// Keep the original file in the database (`--keep-image`).
    #[serde(default)]
    pub keep_image: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub ocr_text: String,
}

/// One line of a review: what was parsed, what is proposed, and the
/// decision (`action`), which a person may edit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReviewLine {
    pub n: usize,
    pub text: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub code_kind: Option<String>,
    pub qty: f64,
    pub unit: String,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(with = "dollars_opt", default)]
    pub unit_price: Option<i64>,
    #[serde(with = "dollars")]
    pub total: i64,
    #[serde(with = "dollars", default)]
    pub discount: i64,
    #[serde(default)]
    pub voided: bool,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub confidence: f64,
    /// `add`, `new` or `skip`.
    pub action: String,
    /// For `add`: the item (id, asset id or name).
    #[serde(default)]
    pub item: Option<String>,
    /// The proposed item's name (for people).
    #[serde(default)]
    pub item_name: Option<String>,
    /// For `new`: the new item's name.
    #[serde(default)]
    pub new_name: Option<String>,
    #[serde(default)]
    pub shelf_life: Option<String>,
    #[serde(default)]
    pub product_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub schema: String,
    pub receipt: Head,
    pub lines: Vec<ReviewLine>,
    #[serde(default)]
    pub skipped: Vec<Skipped>,
}

impl Review {
    /// A review of a parsed receipt with the matcher's proposals.
    pub fn new(head: Head, r: &Receipt, m: &Matcher) -> Self {
        let lines = r
            .lines
            .iter()
            .map(|l| {
                let p = (!l.voided).then(|| m.propose(l));
                ReviewLine {
                    n: l.n,
                    text: l.text.clone(),
                    name: l.name.clone(),
                    code: l.code.clone(),
                    code_kind: l.code_kind.clone(),
                    qty: l.qty,
                    unit: l.unit.clone(),
                    size: l.size.clone(),
                    unit_price: l.unit_price,
                    total: l.total,
                    discount: l.discount,
                    voided: l.voided,
                    tier: p.as_ref().map(|p| p.tier.to_string()),
                    confidence: p.as_ref().map_or(0.0, |p| p.confidence),
                    action: p.as_ref().map_or("skip", |p| p.action).to_string(),
                    item: p.as_ref().and_then(|p| p.item.as_ref().map(|i| i.0.clone())),
                    item_name: p.as_ref().and_then(|p| p.item.as_ref().map(|i| i.1.clone()).or(p.new_name.clone())),
                    new_name: p.as_ref().and_then(|p| p.new_name.clone()),
                    shelf_life: p.as_ref().and_then(|p| p.shelf_life.clone()),
                    product_id: p.and_then(|p| p.product),
                }
            })
            .collect();
        Self { schema: REVIEW_SCHEMA.into(), receipt: head, lines, skipped: r.skipped.clone() }
    }

    /// `--yes`: accept proposals at or above `min`, skip the rest.
    pub fn accept_above(&mut self, min: f64) -> Vec<usize> {
        let mut skipped = Vec::new();
        for l in self.lines.iter_mut().filter(|l| !l.voided && l.action != "skip") {
            if l.confidence + 1e-9 < min {
                l.action = "skip".into();
                skipped.push(l.n);
            }
        }
        skipped
    }

    pub fn parse(text: &str) -> Result<Self> {
        let r: Self = serde_json::from_str(text).map_err(|e| AppError::invalid(format!("review file: {e}")))?;
        if r.schema != REVIEW_SCHEMA {
            return Err(AppError::invalid(format!(
                "not a receipt review (schema '{}', want '{REVIEW_SCHEMA}')",
                r.schema
            )));
        }
        for l in &r.lines {
            if !matches!(l.action.as_str(), "add" | "new" | "skip") {
                return Err(AppError::invalid(format!("line {}: action '{}' (add, new or skip)", l.n, l.action)));
            }
            if l.action == "add" && l.item.as_deref().is_none_or(|i| i.trim().is_empty()) {
                return Err(AppError::invalid(format!("line {}: action add needs an item", l.n)));
            }
            if l.action != "skip" && (l.qty.is_nan() || l.qty <= 0.0) {
                return Err(AppError::invalid(format!("line {}: quantity must be positive", l.n)));
            }
        }
        Ok(r)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default() + "\n"
    }
}

// ---------------------------------------------------------------------------
// Applying a review
// ---------------------------------------------------------------------------

pub struct ApplyOpts<'a> {
    pub force: bool,
    /// Where new items go.
    pub at: Option<&'a str>,
    /// Lots' date when the receipt has none.
    pub today: String,
    /// The original file (with `keep_image`): name and bytes.
    pub image: Option<(String, Vec<u8>)>,
    /// Store the file path.
    pub keep_path: bool,
}

/// One line applied.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedLine {
    pub n: usize,
    pub lot: String,
    pub item_id: String,
    pub item: String,
    pub qty: f64,
    pub unit: String,
    pub created: bool,
    pub product_id: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Applied {
    pub receipt_id: String,
    pub lines: Vec<AppliedLine>,
    pub items_created: usize,
    pub products_created: usize,
    pub products_updated: usize,
    pub warnings: Vec<String>,
}

/// Quantity of a line in the item's unit: a weight converts when it can; a
/// count of a mass/volume item is multiplied by the product's (else the
/// line's) size.
pub fn lot_qty(l: &ReviewLine, item_unit: &str, product: Option<&Product>) -> f64 {
    if l.unit != "ea" {
        return matching::convert(l.qty, &l.unit, item_unit).unwrap_or(l.qty);
    }
    if matches!(matching::unit_info(item_unit), Some((Dim::Mass | Dim::Volume, _))) {
        let size = product
            .and_then(|p| p.size.as_deref().and_then(matching::size_from_name))
            .or_else(|| l.size.as_deref().and_then(matching::size_from_name));
        if let Some(per) = size.and_then(|(q, u)| matching::convert(q, &u, item_unit)) {
            return per * l.qty;
        }
    }
    l.qty
}

fn round6(q: f64) -> f64 {
    (q * 1e6).round() / 1e6
}

/// Apply a review (inside the caller's transaction): lots, new items and
/// store products, then the receipt record.
pub fn apply(db: &Db, review: &Review, o: &ApplyOpts) -> Result<Applied> {
    let h = &review.receipt;
    if !o.force {
        if let Some(old) = by_fingerprint(db, &h.fingerprint)?.first() {
            return Err(AppError::invalid(format!(
                "this receipt was already imported as receipt {} ({}{}); --force imports it again",
                old.id,
                old.purchased_on.as_deref().unwrap_or("undated"),
                old.store_id.as_deref().or(old.chain.as_deref()).map(|s| format!(", {s}")).unwrap_or_default()
            )));
        }
    }
    let mut done = Applied { receipt_id: crate::ulid::new()?, ..Default::default() };
    let rid = done.receipt_id.clone();
    let store = h.store.as_deref().map(|s| sourcing::find_store(db, s)).transpose()?.flatten();
    if h.store.is_some() && store.is_none() {
        done.warnings
            .push(format!("store '{}' is not in this database: lots get it as text", h.store.as_deref().unwrap_or("")));
    }
    let vendor = store.as_ref().map(|s| s.id.clone()).or_else(|| h.store.clone()).or_else(|| h.chain.clone());
    let chain = store.as_ref().map(|s| s.chain.clone()).or_else(|| h.chain.clone());
    let date = match &h.date {
        Some(d) => d.clone(),
        None => {
            done.warnings.push(format!("no purchase date: lots dated {}", o.today));
            o.today.clone()
        }
    };
    let mut tree = Tree::load(db)?;
    let at = o.at.map(|a| tree.resolve_or_create(db, a).map(|r| r.0)).transpose()?;
    let mut products = sourcing::all_products(db)?;
    let stores = sourcing::stores_by_id(db)?;
    let mut new_items: HashMap<String, Item> = HashMap::new();
    let now = now_iso();
    for l in review.lines.iter().filter(|l| !l.voided && l.action != "skip") {
        // the item
        let (item, created) = match l.action.as_str() {
            "add" => (store::resolve_item(db, l.item.as_deref().unwrap_or_default())?, false),
            _ => {
                let name = l.new_name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| l.name.clone());
                let name = if name.trim().is_empty() { l.text.clone() } else { name };
                let key = matching::normalize_name(&name);
                match new_items.get(&key) {
                    Some(i) => (i.clone(), false),
                    None => {
                        let mut i = Item::new(&name)?;
                        i.unit = Some(if l.unit == "ea" { "ea".into() } else { l.unit.clone() });
                        i.location_id = at;
                        i.asset_id = Some(store::next_asset_id(db)?);
                        store::insert_item(db, &i)?;
                        if let Some(t) = l.shelf_life.as_deref().filter(|t| !t.trim().is_empty()) {
                            store::set_item_field(db, &i.id, crate::shelf_life::FIELD, Some(t))?;
                        }
                        store::add_source(db, &i.id, &Source { source: "receipt".into(), source_ref: rid.clone() })?;
                        store::add_event(
                            db,
                            &i.id,
                            "created",
                            json!({"location": tree.opt_path(i.location_id), "via": "receipt", "receipt": rid}),
                        )?;
                        new_items.insert(key, i.clone());
                        done.items_created += 1;
                        (i, true)
                    }
                }
            }
        };
        // the store product
        let food_k = sourcing::food_key(&item.name);
        let in_scope = |p: &Product| match &store {
            Some(s) => {
                p.store_id.as_deref() == Some(s.id.as_str())
                    || (p.store_id.is_none()
                        && sourcing::product_chain(p, &stores)
                            .is_some_and(|c| sourcing::chain_key(&c) == sourcing::chain_key(&s.chain)))
            }
            None => chain.as_ref().is_some_and(|c| {
                p.store_id.is_none()
                    && p.chain.as_deref().is_some_and(|pc| sourcing::chain_key(pc) == sourcing::chain_key(c))
            }),
        };
        let known = l.product_id.and_then(|id| products.iter().position(|p| p.id == id)).or_else(|| {
            products.iter().position(|p| {
                in_scope(p)
                    && (p.item_id.as_deref() == Some(item.id.as_str())
                        || l.code.as_deref().is_some_and(|c| code_matches(p, c))
                        || p.food_key == food_k)
            })
        });
        let seen_price = l.unit_price.or_else(|| (l.qty > 0.0).then(|| (l.total as f64 / l.qty).round() as i64));
        let product_id = match known {
            Some(ix) => {
                let p = &mut products[ix];
                if p.price_seen.as_deref().is_none_or(|s| s <= date.as_str()) {
                    p.price = seen_price.or(p.price);
                    p.price_seen = Some(date.clone());
                }
                if let (Some(code), Some(kind)) = (&l.code, &l.code_kind) {
                    let slot = match kind.as_str() {
                        "plu" => &mut p.plu,
                        "upc" => &mut p.upc,
                        _ => &mut p.sku,
                    };
                    if slot.is_none() {
                        *slot = Some(code.clone());
                    }
                }
                if p.item_id.is_none() {
                    p.item_id = Some(item.id.clone());
                }
                p.updated_at.clone_from(&now);
                sourcing::update_product(db, p)?;
                done.products_updated += 1;
                Some(p.id)
            }
            None if store.is_some() || chain.is_some() => {
                let display = if l.name.trim().is_empty() { l.text.clone() } else { l.name.clone() };
                let mut p = Product {
                    food: item.name.clone(),
                    food_key: food_k.clone(),
                    item_id: Some(item.id.clone()),
                    store_id: store.as_ref().map(|s| s.id.clone()),
                    chain: if store.is_some() { None } else { chain.clone() },
                    name: display.clone(),
                    size: l.size.clone().or_else(|| (l.unit != "ea").then(|| format!("1 {}", l.unit))),
                    price: seen_price,
                    price_seen: seen_price.map(|_| date.clone()),
                    section: sourcing::guess_section(None, None, Some(&display), Some(&item.name)).map(|g| g.section),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    ..Default::default()
                };
                if let (Some(code), Some(kind)) = (&l.code, &l.code_kind) {
                    match kind.as_str() {
                        "plu" => p.plu = Some(code.clone()),
                        "upc" => p.upc = Some(code.clone()),
                        _ => p.sku = Some(code.clone()),
                    }
                }
                p.id = sourcing::insert_product(db, &p)?;
                done.products_created += 1;
                let id = p.id;
                products.push(p);
                Some(id)
            }
            None => None,
        };
        // the lot
        let unit = item.unit.clone().unwrap_or_else(|| "ea".into());
        let product = product_id.and_then(|id| products.iter().find(|p| p.id == id));
        let qty = round6(lot_qty(l, &unit, product));
        if l.unit != "ea" && matching::convert(1.0, &l.unit, &unit).is_none() {
            done.warnings.push(format!(
                "line {}: {} {} of {} counted as {} {unit} (no conversion)",
                l.n,
                crate::util::fmt_qty(l.qty),
                l.unit,
                item.name,
                crate::util::fmt_qty(qty)
            ));
        }
        let lot = Lot {
            id: crate::ulid::new()?,
            item_id: item.id.clone(),
            qty,
            acquired_at: Some(date.clone()),
            status: "sealed".into(),
            created_at: now.clone(),
            unit_price: (qty > 0.0).then(|| (l.total as f64 / qty).round() as i64),
            source_ref: Some(format!("{}{}", lot_prefix(&rid), l.n)),
            vendor: vendor.clone(),
            ..Default::default()
        };
        store::insert_lot(db, &lot)?;
        store::sync_lot_quantity(db, &item.id)?;
        store::add_event(
            db,
            &item.id,
            "lot_added",
            json!({"lot": lot.id, "qty": lot.qty, "receipt": rid, "line": l.n}),
        )?;
        done.lines.push(AppliedLine {
            n: l.n,
            lot: lot.id,
            item_id: item.id.clone(),
            item: item.name.clone(),
            qty,
            unit,
            created,
            product_id,
        });
    }
    let base64 = |b: &[u8]| {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(b)
    };
    let rec = StoredReceipt {
        id: rid,
        fingerprint: h.fingerprint.clone(),
        store_id: store.as_ref().map(|s| s.id.clone()),
        chain,
        purchased_on: Some(date),
        purchased_time: h.time.clone(),
        subtotal: h.subtotal,
        tax: h.tax,
        total: h.total,
        lines: done.lines.len() as i64,
        ocr_text: h.ocr_text.clone(),
        source_path: h.file.clone().filter(|_| o.keep_path),
        image_name: o.image.as_ref().map(|i| i.0.clone()),
        image: o.image.as_ref().map(|i| base64(&i.1)),
        created_at: now,
    };
    insert_receipt(db, &rec)?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noun_first_names_are_read_noun_last() {
        let t = tempfile::TempDir::new().unwrap();
        let db = Db::open_plain(&t.path().join("m.db")).unwrap();
        crate::migrations::migrate(&db).unwrap();
        let m = Matcher::load(&db, &Types::builtin(), None, None, 0.82).unwrap();
        assert_eq!(m.food_order("Avocado Hass"), "Hass Avocado");
        assert_eq!(m.food_order("Onion Yellow"), "Yellow Onion");
        assert_eq!(m.food_order("Organic Bananas"), "Organic Bananas");
        assert_eq!(m.food_order("Paper Towels"), "Paper Towels");
        let l = receipt::parse("AVOCADO HASS 3.75\n", None).lines.remove(0);
        assert_eq!(l.name, "Avocado Hass");
        let p = m.propose(&l);
        assert_eq!((p.action, p.tier, p.new_name.as_deref()), ("new", "new", Some("Hass Avocado")));
        assert_eq!((p.confidence, p.shelf_life.as_deref()), (0.8, Some("avocado")));
    }

    #[test]
    fn codes_compare_as_gtins() {
        let p = Product { plu: Some("4011".into()), upc: Some("0 12345 67890 5".into()), ..Default::default() };
        assert!(code_matches(&p, "4011"));
        assert!(code_matches(&p, "012345678905"));
        assert!(code_matches(&p, "0012345678905"), "EAN-13 of the same UPC");
        assert!(!code_matches(&p, "94011"));
    }

    #[test]
    fn quantities_convert_to_the_item_unit() {
        let l = ReviewLine { qty: 2.0, unit: "ea".into(), size: Some("16 oz".into()), ..Default::default() };
        assert_eq!(lot_qty(&l, "ea", None), 2.0);
        assert!((lot_qty(&l, "oz", None) - 32.0).abs() < 1e-9);
        let bag = Product { size: Some("1 lb".into()), ..Default::default() };
        assert!((lot_qty(&l, "g", Some(&bag)) - 907.18474).abs() < 1e-3, "the product's size wins");
        let w = ReviewLine { qty: 2.0, unit: "lb".into(), ..Default::default() };
        assert!((lot_qty(&w, "kg", None) - 0.907).abs() < 1e-3);
        assert_eq!(lot_qty(&w, "ea", None), 2.0, "no conversion: the line's quantity");
    }

    #[test]
    fn review_files_round_trip() {
        let r = receipt::parse(include_str!("../tests/fixtures/receipts/aldi.txt"), None);
        let lines = r
            .lines
            .iter()
            .map(|l| ReviewLine {
                n: l.n,
                text: l.text.clone(),
                name: l.name.clone(),
                qty: l.qty,
                unit: l.unit.clone(),
                total: l.total,
                discount: l.discount,
                unit_price: l.unit_price,
                voided: l.voided,
                action: if l.voided { "skip".into() } else { "new".into() },
                new_name: Some(l.name.clone()),
                ..Default::default()
            })
            .collect();
        let review = Review {
            schema: REVIEW_SCHEMA.into(),
            receipt: Head { fingerprint: "f".into(), total: r.total, ocr_text: "x".into(), ..Default::default() },
            lines,
            skipped: r.skipped.clone(),
        };
        let back = Review::parse(&review.to_json()).unwrap();
        assert_eq!(back, review);
        let mut bad = review.clone();
        bad.lines[0].action = "maybe".into();
        assert!(Review::parse(&bad.to_json()).is_err());
        bad.schema = "other".into();
        assert!(Review::parse(&bad.to_json()).is_err());
    }
}
