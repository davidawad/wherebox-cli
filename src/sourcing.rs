//! Store sourcing (docs/SPEC.md, "Stores and sourcing"): stores, their
//! products per food, in-store sections and layouts, and the choice of
//! product for a list entry at the store that serves a profile.
//!
//! Stores live in each property's database: a profile's stores are the ones
//! near that property. A store product covers one store (`store_id`) or
//! every store of a chain (`chain`). Nothing here touches the network: URLs
//! and category text are parsed, never fetched.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::{json, Value as Json};

use crate::db::{int, opt_int, opt_text, text, Db, Row, RowExt, Value};
use crate::error::{AppError, Result};
use crate::matching;
use crate::util::now_iso;

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

/// The fixed section vocabulary: (id, title, aliases).
pub const SECTIONS: &[(&str, &str, &[&str])] = &[
    ("produce", "Produce", &["fruit", "vegetables", "fruits & vegetables"]),
    ("bulk", "Bulk", &["bulk bins", "bins"]),
    ("meat", "Meat", &["butcher"]),
    ("seafood", "Seafood", &["fish"]),
    ("deli", "Deli", &["prepared foods"]),
    ("bakery", "Bakery", &["bread"]),
    ("dairy", "Dairy", &[]),
    ("eggs", "Eggs", &["egg"]),
    ("frozen", "Frozen", &["freezer"]),
    ("pantry", "Pantry", &["dry goods", "pantry/dry goods", "pantry-dry-goods", "dry", "grocery"]),
    ("canned", "Canned", &["canned goods", "cans"]),
    ("baking", "Baking", &[]),
    ("spices", "Spices", &["spice", "herbs & spices", "herbs-spices", "seasonings"]),
    (
        "oils-vinegar",
        "Oils & vinegar",
        &["oils", "oil", "vinegar", "oils/vinegar", "oils & vinegar", "oils-and-vinegar"],
    ),
    ("snacks", "Snacks", &["snack"]),
    ("beverages", "Beverages", &["beverage", "drinks"]),
    (
        "supplements",
        "Supplements",
        &["vitamins", "supplements/vitamins", "supplements-vitamins", "vitamins & supplements"],
    ),
    ("household", "Household", &["cleaning"]),
    ("health-beauty", "Health & beauty", &["health/beauty", "health", "beauty", "health & beauty", "personal care"]),
    ("checkout", "Checkout", &["registers"]),
    ("special-buys", "Special buys", &["center", "center/special-buys", "center-special-buys", "specials"]),
];

/// The title of the group of entries with no section (or no product).
pub const UNSORTED: &str = "Ask / unsorted";

fn section_slug(s: &str) -> String {
    crate::util::slugify(&s.replace('&', " and ").replace(" and ", " "))
}

/// The canonical id of a section and whether it is custom (not in the
/// vocabulary): `Dry Goods` -> `pantry`, `Hot Bar` -> `hot-bar` (custom).
pub fn section_id(input: &str) -> Option<(String, bool)> {
    let t = input.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    let slug = section_slug(&t);
    for (id, _, aliases) in SECTIONS {
        if *id == t || *id == slug || aliases.iter().any(|a| *a == t || section_slug(a) == slug) {
            return Some(((*id).to_string(), false));
        }
    }
    Some((slug, true))
}

/// A section's display title (`oils-vinegar` -> `Oils & vinegar`; a custom
/// `hot-bar` -> `Hot bar`).
pub fn section_title(id: &str) -> String {
    if let Some((_, t, _)) = SECTIONS.iter().find(|(s, _, _)| *s == id) {
        return (*t).to_string();
    }
    let words = id.replace('-', " ");
    let mut c = words.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

// ---------------------------------------------------------------------------
// Section guess
// ---------------------------------------------------------------------------

/// Phrases that decide a section before any single word does.
const PHRASES: &[(&str, &str)] = &[
    ("peanut butter", "pantry"),
    ("almond butter", "pantry"),
    ("cashew butter", "pantry"),
    ("nut butter", "pantry"),
    ("sunflower butter", "pantry"),
    ("apple butter", "pantry"),
    ("coconut milk", "canned"),
    ("olive oil", "oils-vinegar"),
    ("baking soda", "baking"),
    ("baking powder", "baking"),
    ("cocoa powder", "baking"),
    ("chocolate chip", "baking"),
    ("protein powder", "supplements"),
    ("trail mix", "snacks"),
    ("ice cream", "frozen"),
    ("sour cream", "dairy"),
    ("cream cheese", "dairy"),
    ("paper towel", "household"),
    ("toilet paper", "household"),
    ("trash bag", "household"),
    ("dish soap", "household"),
    ("sparkling water", "beverages"),
    ("hot sauce", "pantry"),
];

/// Words that put a product in a section wherever they are.
const PLACE_WORDS: &[(&str, &str)] =
    &[("frozen", "frozen"), ("canned", "canned"), ("tinned", "canned"), ("can", "canned"), ("bulk", "bulk")];

/// Word table, in priority order (singular forms).
const WORDS: &[(&str, &[&str])] = &[
    (
        "spices",
        &[
            "spice",
            "seasoning",
            "cinnamon",
            "paprika",
            "cumin",
            "oregano",
            "turmeric",
            "salt",
            "peppercorn",
            "nutmeg",
            "clove",
            "powder",
            "thyme",
            "rosemary",
        ],
    ),
    ("oils-vinegar", &["oil", "vinegar"]),
    ("baking", &["flour", "sugar", "yeast", "cocoa", "baking", "vanilla", "cornstarch"]),
    ("supplements", &["supplement", "vitamin", "capsule", "softgel", "probiotic", "multivitamin"]),
    (
        "health-beauty",
        &["shampoo", "soap", "lotion", "toothpaste", "deodorant", "conditioner", "sunscreen", "beauty", "floss"],
    ),
    ("household", &["detergent", "cleaner", "towel", "foil", "trash", "sponge", "household", "cleaning", "napkin"]),
    ("beverages", &["juice", "soda", "coffee", "tea", "kombucha", "beverage", "drink", "seltzer", "beer", "wine"]),
    ("snacks", &["snack", "chip", "cracker", "pretzel", "popcorn", "bar", "jerky"]),
    ("bakery", &["bread", "bagel", "tortilla", "muffin", "croissant", "bun", "bakery", "baguette", "pita"]),
    ("deli", &["deli", "hummus", "salami", "prosciutto"]),
    ("eggs", &["egg"]),
    ("dairy", &["milk", "cheese", "yogurt", "butter", "cream", "kefir", "dairy"]),
    ("seafood", &["fish", "salmon", "shrimp", "cod", "tilapia", "crab", "scallop", "seafood", "halibut", "tuna"]),
    ("meat", &["meat", "beef", "chicken", "pork", "turkey", "lamb", "bacon", "sausage", "steak", "ham", "chorizo"]),
    (
        "produce",
        &[
            "produce",
            "fruit",
            "vegetable",
            "veggie",
            "banana",
            "apple",
            "avocado",
            "lime",
            "lemon",
            "orange",
            "berry",
            "blueberry",
            "strawberry",
            "raspberry",
            "blackberry",
            "grape",
            "lettuce",
            "spinach",
            "kale",
            "onion",
            "garlic",
            "potato",
            "tomato",
            "carrot",
            "cucumber",
            "ginger",
            "cilantro",
            "parsley",
            "basil",
            "mushroom",
            "celery",
            "broccoli",
            "zucchini",
            "cherry",
            "mango",
            "pear",
            "peach",
            "herb",
        ],
    ),
    (
        "pantry",
        &[
            "rice",
            "pasta",
            "bean",
            "lentil",
            "oat",
            "cereal",
            "grain",
            "quinoa",
            "walnut",
            "almond",
            "pecan",
            "cashew",
            "pistachio",
            "peanut",
            "nut",
            "seed",
            "sauce",
            "honey",
            "broth",
            "soup",
            "jam",
            "syrup",
            "noodle",
            "chickpea",
            "salsa",
            "ketchup",
            "mustard",
            "mayonnaise",
            "pantry",
        ],
    ),
];

/// Lower-case words of a text (letters and digits), with their singulars.
fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !w.chars().all(|c| c.is_ascii_digit()))
        .map(matching::singular)
        .collect()
}

fn phrase_hit(ws: &[String]) -> Option<&'static str> {
    let joined = format!(" {} ", ws.join(" "));
    PHRASES.iter().find(|(p, _)| joined.contains(&format!(" {} ", words(p).join(" ")))).map(|(_, s)| *s)
}

fn word_section(w: &str) -> Option<&'static str> {
    WORDS.iter().find(|(_, ws)| ws.contains(&w)).map(|(s, _)| *s)
}

/// The section a vocabulary word or alias names (`dairy`, `drinks`).
fn named_section(w: &str) -> Option<String> {
    section_id(w).filter(|(_, custom)| !custom).map(|(id, _)| id).filter(|id| !matches!(id.as_str(), "checkout"))
}

/// The words of a URL's path (host, query and fragment dropped).
fn url_words(url: &str) -> Vec<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.split_once('/').map_or("", |(_, p)| p);
    let path = path.split(['?', '#']).next().unwrap_or("");
    words(path)
}

/// A guessed section and what it was guessed from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Guess {
    pub section: String,
    /// `category`, `url`, `name` or `food`.
    pub basis: &'static str,
}

/// Guess a section from a store's category text, a product URL, the
/// product name and the food, in that order (see the spec's table).
pub fn guess_section(
    category: Option<&str>,
    url: Option<&str>,
    name: Option<&str>,
    food: Option<&str>,
) -> Option<Guess> {
    let hit = |section: &str, basis| Some(Guess { section: section.to_string(), basis });
    // category text and URL paths: phrases, place words, the leftmost
    // section name, then any word of the table
    for (basis, ws) in [("category", category.map(words)), ("url", url.map(url_words))] {
        let Some(ws) = ws.filter(|w| !w.is_empty()) else { continue };
        if let Some(s) = phrase_hit(&ws) {
            return hit(s, basis);
        }
        if let Some(s) = ws.iter().find_map(|w| PLACE_WORDS.iter().find(|(p, _)| p == w).map(|(_, s)| *s)) {
            return hit(s, basis);
        }
        if let Some(s) = ws.iter().find_map(|w| named_section(w)) {
            return hit(&s, basis);
        }
        if let Some(s) = WORDS.iter().find(|(_, list)| ws.iter().any(|w| list.contains(&w.as_str()))).map(|(s, _)| *s) {
            return hit(s, basis);
        }
    }
    // names: phrases, place words, the head noun, then any word
    for (basis, n) in [("name", name), ("food", food)] {
        let Some(n) = n.filter(|n| !n.trim().is_empty()) else { continue };
        let ws = words(n);
        if let Some(s) = phrase_hit(&ws) {
            return hit(s, basis);
        }
        if let Some(s) = ws.iter().find_map(|w| PLACE_WORDS.iter().find(|(p, _)| p == w).map(|(_, s)| *s)) {
            return hit(s, basis);
        }
        let segs = matching::segments(n);
        let heads = segs.iter().filter(|s| !s.trailing).filter_map(|s| s.head.as_deref());
        if let Some(s) = heads.clone().find_map(word_section) {
            return hit(s, basis);
        }
        if let Some(s) = heads.clone().find_map(named_section) {
            return hit(&s, basis);
        }
        if let Some(s) = WORDS.iter().find(|(_, list)| ws.iter().any(|w| list.contains(&w.as_str()))).map(|(s, _)| *s) {
            return hit(s, basis);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Layouts
// ---------------------------------------------------------------------------

/// One stop of a layout: a section and, optionally, its aisles.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stop {
    pub section: String,
    pub aisles: Option<String>,
}

/// Sections in walking order, entrance to checkout.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Layout(pub Vec<Stop>);

/// The generic default: produce first, cold and frozen last, checkout at the end.
pub const DEFAULT_LAYOUT: &str = "produce, bulk, bakery, deli, meat, seafood, pantry, canned, baking, spices, \
    oils-vinegar, snacks, beverages, supplements, health-beauty, household, special-buys, eggs, dairy, frozen, checkout";

impl Layout {
    /// `produce, bulk=1-2, pantry=3-7, checkout` (sections may repeat only once).
    pub fn parse(s: &str) -> Result<Self> {
        let mut stops: Vec<Stop> = Vec::new();
        for part in s.split([',', '\n']).map(str::trim).filter(|p| !p.is_empty()) {
            let (sec, aisles) = match part.split_once(['=', ':']) {
                Some((a, b)) => (a.trim(), Some(b.trim().to_string()).filter(|b| !b.is_empty())),
                None => (part, None),
            };
            let (id, _) = section_id(sec).ok_or_else(|| AppError::invalid(format!("empty section in layout '{s}'")))?;
            if stops.iter().any(|x| x.section == id) {
                return Err(AppError::invalid(format!("section '{id}' is twice in the layout")));
            }
            stops.push(Stop { section: id, aisles });
        }
        if stops.is_empty() {
            return Err(AppError::invalid("a layout needs at least one section"));
        }
        Ok(Self(stops))
    }

    pub fn default_layout() -> Self {
        Self::parse(DEFAULT_LAYOUT).expect("the default layout parses")
    }

    /// The canonical text form (what is stored).
    pub fn to_text(&self) -> String {
        self.0
            .iter()
            .map(|s| match &s.aisles {
                Some(a) => format!("{}={a}", s.section),
                None => s.section.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn position(&self, section: &str) -> Option<usize> {
        self.0.iter().position(|s| s.section == section)
    }

    pub fn aisles(&self, section: &str) -> Option<&str> {
        self.0.iter().find(|s| s.section == section).and_then(|s| s.aisles.as_deref())
    }

    /// Walking order of a section: its place in the layout; a section not
    /// in the layout comes just before `checkout` (else last).
    pub fn order(&self, section: &str) -> (usize, u8) {
        match self.position(section) {
            Some(p) => (p, 1),
            None => (self.position("checkout").unwrap_or(self.0.len()), 0),
        }
    }
}

// ---------------------------------------------------------------------------
// Stores
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Store {
    pub id: String,
    pub chain: String,
    pub name: String,
    pub address: Option<String>,
    pub website: Option<String>,
    pub notes: Option<String>,
    /// Its own layout (canonical text), else the chain's or the default.
    pub layout: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Chains compare case-insensitively, spaces collapsed.
pub fn chain_key(chain: &str) -> String {
    chain.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Store ids are slugs: letters, digits, `-`, `_`.
pub fn validate_store_id(id: &str) -> Result<String> {
    let t = id.trim().to_lowercase();
    if !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
        Ok(t)
    } else {
        Err(AppError::usage(format!(
            "invalid store id '{id}' (letters, digits, '-', '_'; e.g. '{}')",
            crate::util::slugify(id)
        )))
    }
}

const STORE_COLS: &str = "id, chain, name, address, website, notes, layout, created_at, updated_at";

fn store_from(r: &Row) -> Store {
    Store {
        id: r.s(0).unwrap_or_default(),
        chain: r.s(1).unwrap_or_default(),
        name: r.s(2).unwrap_or_default(),
        address: r.s(3),
        website: r.s(4),
        notes: r.s(5),
        layout: r.s(6),
        created_at: r.s(7).unwrap_or_default(),
        updated_at: r.s(8).unwrap_or_default(),
    }
}

fn store_params(s: &Store) -> Vec<Value> {
    vec![
        text(&s.id),
        text(&s.chain),
        text(&s.name),
        opt_text(s.address.as_ref()),
        opt_text(s.website.as_ref()),
        opt_text(s.notes.as_ref()),
        opt_text(s.layout.as_ref()),
        text(&s.created_at),
        text(&s.updated_at),
    ]
}

fn check_store(s: &Store) -> Result<()> {
    validate_store_id(&s.id)?;
    if s.chain.trim().is_empty() {
        return Err(AppError::invalid(format!("store '{}' needs a chain", s.id)));
    }
    if s.name.trim().is_empty() {
        return Err(AppError::invalid(format!("store '{}' needs a name", s.id)));
    }
    Ok(())
}

pub fn insert_store(db: &Db, s: &Store) -> Result<()> {
    check_store(s)?;
    if get_store(db, &s.id)?.is_some() {
        return Err(AppError::invalid(format!("store '{}' already exists", s.id)));
    }
    db.execute(
        &format!("INSERT INTO stores ({STORE_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"),
        &store_params(s),
    )
    .map(|_| ())
}

pub fn update_store(db: &Db, s: &Store) -> Result<()> {
    check_store(s)?;
    db.execute(
        "UPDATE stores SET chain = ?2, name = ?3, address = ?4, website = ?5, notes = ?6, layout = ?7, \
         created_at = ?8, updated_at = ?9 WHERE id = ?1",
        &store_params(s),
    )
    .map(|_| ())
}

pub fn get_store(db: &Db, id: &str) -> Result<Option<Store>> {
    Ok(db
        .query_opt(&format!("SELECT {STORE_COLS} FROM stores WHERE id = ?1"), &[text(id.trim().to_lowercase())])?
        .map(|r| store_from(&r)))
}

/// Every store, by chain then name then id.
pub fn all_stores(db: &Db) -> Result<Vec<Store>> {
    let mut v: Vec<Store> =
        db.query(&format!("SELECT {STORE_COLS} FROM stores"), &[])?.iter().map(store_from).collect();
    v.sort_by_key(|s| (chain_key(&s.chain), s.name.to_lowercase(), s.id.clone()));
    Ok(v)
}

/// A store by id, else by unique name (case-insensitive).
pub fn find_store(db: &Db, spec: &str) -> Result<Option<Store>> {
    if let Some(s) = get_store(db, spec)? {
        return Ok(Some(s));
    }
    let want = spec.trim().to_lowercase();
    let hits: Vec<Store> = all_stores(db)?.into_iter().filter(|s| s.name.to_lowercase() == want).collect();
    match hits.len() {
        0 => Ok(None),
        1 => Ok(hits.into_iter().next()),
        n => Err(AppError::invalid(format!("'{spec}' names {n} stores; use the store id"))),
    }
}

pub fn resolve_store(db: &Db, spec: &str) -> Result<Store> {
    find_store(db, spec)?.ok_or_else(|| AppError::not_found(format!("no store '{spec}' (see `wherebox store ls`)")))
}

pub fn delete_store(db: &Db, id: &str) -> Result<()> {
    db.execute("DELETE FROM store_products WHERE store_id = ?1", &[text(id)])?;
    db.execute("DELETE FROM stores WHERE id = ?1", &[text(id)]).map(|_| ())
}

/// A lot's vendor: a store's id when `v` names one, else the text (empty: none).
pub fn vendor_ref(db: &Db, v: &str) -> Result<Option<String>> {
    let t = v.trim();
    if t.is_empty() {
        return Ok(None);
    }
    Ok(Some(find_store(db, t).ok().flatten().map_or_else(|| t.to_string(), |s| s.id)))
}

/// A chain's default layout (canonical text).
pub fn chain_layout(db: &Db, chain: &str) -> Result<Option<String>> {
    Ok(db
        .query_opt("SELECT layout FROM chain_layouts WHERE chain = ?1", &[text(chain_key(chain))])?
        .and_then(|r| r.s(0)))
}

/// Every chain layout: (chain key, layout).
pub fn chain_layouts(db: &Db) -> Result<Vec<(String, String)>> {
    let mut v: Vec<(String, String)> = db
        .query("SELECT chain, layout FROM chain_layouts", &[])?
        .iter()
        .map(|r| (r.s(0).unwrap_or_default(), r.s(1).unwrap_or_default()))
        .collect();
    v.sort();
    Ok(v)
}

pub fn set_chain_layout(db: &Db, chain: &str, layout: Option<&Layout>) -> Result<()> {
    let key = chain_key(chain);
    if key.is_empty() {
        return Err(AppError::usage("--chain must not be empty"));
    }
    db.execute("DELETE FROM chain_layouts WHERE chain = ?1", &[text(&key)])?;
    if let Some(l) = layout {
        db.execute(
            "INSERT INTO chain_layouts (chain, layout, updated_at) VALUES (?1, ?2, ?3)",
            &[text(&key), text(l.to_text()), text(now_iso())],
        )?;
    }
    Ok(())
}

/// A store's layout and where it came from: `store`, `chain` or `default`.
pub fn effective_layout(db: &Db, store: &Store) -> Result<(Layout, &'static str)> {
    if let Some(l) = store.layout.as_deref().and_then(|l| Layout::parse(l).ok()) {
        return Ok((l, "store"));
    }
    if let Some(l) = chain_layout(db, &store.chain)?.and_then(|l| Layout::parse(&l).ok()) {
        return Ok((l, "chain"));
    }
    Ok((Layout::default_layout(), "default"))
}

// ---------------------------------------------------------------------------
// Store products
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Product {
    pub id: i64,
    /// The canonical food, as typed (`walnuts`).
    pub food: String,
    /// Its normalized name (see [`matching::normalize_name`]).
    pub food_key: String,
    pub item_id: Option<String>,
    pub store_id: Option<String>,
    pub chain: Option<String>,
    pub name: String,
    pub brand: Option<String>,
    pub size: Option<String>,
    /// USD cents.
    pub price: Option<i64>,
    pub price_seen: Option<String>,
    pub url: Option<String>,
    pub sku: Option<String>,
    pub plu: Option<String>,
    pub upc: Option<String>,
    pub section: Option<String>,
    pub aisle: Option<String>,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The normalized key of a food name (falls back to lower case when every
/// word is a stopword).
pub fn food_key(food: &str) -> String {
    let k = matching::normalize_name(food);
    if k.is_empty() {
        food.trim().to_lowercase()
    } else {
        k
    }
}

const PRODUCT_COLS: &str = "id, food, food_key, item_id, store_id, chain, name, brand, size, price, price_seen, url, \
    sku, plu, upc, section, aisle, notes, created_at, updated_at";

fn product_from(r: &Row) -> Product {
    Product {
        id: r.i(0).unwrap_or_default(),
        food: r.s(1).unwrap_or_default(),
        food_key: r.s(2).unwrap_or_default(),
        item_id: r.s(3),
        store_id: r.s(4),
        chain: r.s(5),
        name: r.s(6).unwrap_or_default(),
        brand: r.s(7),
        size: r.s(8),
        price: r.i(9),
        price_seen: r.s(10),
        url: r.s(11),
        sku: r.s(12),
        plu: r.s(13),
        upc: r.s(14),
        section: r.s(15),
        aisle: r.s(16),
        notes: r.s(17),
        created_at: r.s(18).unwrap_or_default(),
        updated_at: r.s(19).unwrap_or_default(),
    }
}

fn product_params(p: &Product) -> Vec<Value> {
    vec![
        text(&p.food),
        text(&p.food_key),
        opt_text(p.item_id.as_ref()),
        opt_text(p.store_id.as_ref()),
        opt_text(p.chain.as_ref()),
        text(&p.name),
        opt_text(p.brand.as_ref()),
        opt_text(p.size.as_ref()),
        opt_int(p.price),
        opt_text(p.price_seen.as_ref()),
        opt_text(p.url.as_ref()),
        opt_text(p.sku.as_ref()),
        opt_text(p.plu.as_ref()),
        opt_text(p.upc.as_ref()),
        opt_text(p.section.as_ref()),
        opt_text(p.aisle.as_ref()),
        opt_text(p.notes.as_ref()),
        text(&p.created_at),
        text(&p.updated_at),
    ]
}

fn check_product(db: &Db, p: &Product) -> Result<()> {
    if p.food.trim().is_empty() {
        return Err(AppError::invalid("a store product needs a food"));
    }
    if p.name.trim().is_empty() {
        return Err(AppError::invalid("a store product needs a name"));
    }
    match (&p.store_id, &p.chain) {
        (None, None) => return Err(AppError::usage("a store product needs --store or --chain")),
        (Some(s), _) if get_store(db, s)?.is_none() => {
            return Err(AppError::not_found(format!("no store '{s}' (see `wherebox store ls`)")))
        }
        _ => {}
    }
    Ok(())
}

/// Insert a product; its id is assigned unless `p.id` is set (and free).
pub fn insert_product(db: &Db, p: &Product) -> Result<i64> {
    check_product(db, p)?;
    let cols = PRODUCT_COLS.split_once(", ").map_or(PRODUCT_COLS, |(_, rest)| rest);
    let ph = (1..=19).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
    if p.id > 0 && get_product(db, p.id)?.is_none() {
        let mut params = vec![int(p.id)];
        params.extend(product_params(p));
        let ph = (1..=20).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
        db.execute(&format!("INSERT INTO store_products ({PRODUCT_COLS}) VALUES ({ph})"), &params)?;
        return Ok(p.id);
    }
    db.execute(&format!("INSERT INTO store_products ({cols}) VALUES ({ph})"), &product_params(p))?;
    Ok(db.last_insert_rowid())
}

pub fn update_product(db: &Db, p: &Product) -> Result<()> {
    check_product(db, p)?;
    let sets = PRODUCT_COLS
        .split(", ")
        .skip(1)
        .enumerate()
        .map(|(n, c)| format!("{c} = ?{}", n + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let mut params = product_params(p);
    params.push(int(p.id));
    db.execute(&format!("UPDATE store_products SET {sets} WHERE id = ?20"), &params).map(|_| ())
}

pub fn get_product(db: &Db, id: i64) -> Result<Option<Product>> {
    Ok(db
        .query_opt(&format!("SELECT {PRODUCT_COLS} FROM store_products WHERE id = ?1"), &[int(id)])?
        .map(|r| product_from(&r)))
}

pub fn resolve_product(db: &Db, spec: &str) -> Result<Product> {
    let id: i64 = spec
        .trim()
        .trim_start_matches('#')
        .parse()
        .map_err(|_| AppError::usage(format!("'{spec}' is not a store product id (see `wherebox source ls`)")))?;
    get_product(db, id)?.ok_or_else(|| AppError::not_found(format!("no store product {id}")))
}

pub fn delete_product(db: &Db, id: i64) -> Result<()> {
    db.execute("DELETE FROM store_products WHERE id = ?1", &[int(id)]).map(|_| ())
}

/// Every store product, by food then id.
pub fn all_products(db: &Db) -> Result<Vec<Product>> {
    let mut v: Vec<Product> =
        db.query(&format!("SELECT {PRODUCT_COLS} FROM store_products"), &[])?.iter().map(product_from).collect();
    v.sort_by(|a, b| a.food_key.cmp(&b.food_key).then(a.id.cmp(&b.id)));
    Ok(v)
}

// ---------------------------------------------------------------------------
// Choosing a product
// ---------------------------------------------------------------------------

/// How a product is linked to an entry (strongest first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Link {
    Name,
    Item,
    HeadNoun,
}

impl Link {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Item => "item",
            Self::HeadNoun => "head noun",
        }
    }
}

/// Where the chosen product is, relative to the store in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Store,
    Chain,
    Other,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::Chain => "chain",
            Self::Other => "other",
        }
    }
}

/// What an entry wants: its names (own name and `any`) and the items it matched.
#[derive(Debug, Clone, Default)]
pub struct Want {
    pub names: Vec<String>,
    pub items: Vec<String>,
}

/// How `p` is linked to `w`, if at all.
pub fn link(p: &Product, w: &Want) -> Option<Link> {
    if w.names.iter().any(|n| !p.food_key.is_empty() && food_key(n) == p.food_key) {
        return Some(Link::Name);
    }
    if p.item_id.as_ref().is_some_and(|i| w.items.contains(i)) {
        return Some(Link::Item);
    }
    let head = |a: &str, b: &str| {
        let h = matching::head_match(&matching::name_tokens(a), &matching::segments(b));
        h.score >= 0.85 && !h.compound
    };
    w.names.iter().any(|n| head(n, &p.food) || head(&p.food, n)).then_some(Link::HeadNoun)
}

/// The stores of a database by id.
pub type Stores = HashMap<String, Store>;

pub fn stores_by_id(db: &Db) -> Result<Stores> {
    Ok(all_stores(db)?.into_iter().map(|s| (s.id.clone(), s)).collect())
}

/// The chain a product belongs to: its own, else its store's.
pub fn product_chain(p: &Product, stores: &Stores) -> Option<String> {
    p.chain.clone().or_else(|| p.store_id.as_ref().and_then(|s| stores.get(s)).map(|s| s.chain.clone()))
}

/// A product chosen for an entry.
#[derive(Debug, Clone)]
pub struct Choice {
    pub product: Product,
    pub tier: Tier,
    pub link: Link,
}

/// The store used for a profile, and the profile's other stores in order.
#[derive(Debug, Clone, Default)]
pub struct Shop {
    pub store: Option<Store>,
    pub others: Vec<String>,
}

/// Choose a product for `w` at `shop`: that store, else its chain, else
/// the profile's other stores in order, else any store; ties to the
/// stronger link, then the lowest id.
pub fn choose(products: &[Product], stores: &Stores, shop: &Shop, w: &Want) -> Option<Choice> {
    let rank = |p: &Product| -> (Tier, usize) {
        let chain = product_chain(p, stores).map(|c| chain_key(&c));
        if let Some(s) = &shop.store {
            if p.store_id.as_deref() == Some(s.id.as_str()) {
                return (Tier::Store, 0);
            }
            if chain.as_deref() == Some(chain_key(&s.chain).as_str()) {
                return (Tier::Chain, 0);
            }
        }
        let pos = shop.others.iter().position(|o| {
            p.store_id.as_deref() == Some(o.as_str())
                || stores.get(o).is_some_and(|s| chain.as_deref() == Some(chain_key(&s.chain).as_str()))
        });
        (Tier::Other, pos.unwrap_or(shop.others.len()))
    };
    products
        .iter()
        .filter_map(|p| link(p, w).map(|l| (p, l)))
        .min_by_key(|(p, l)| (rank(p), *l, p.id))
        .map(|(p, l)| Choice { product: p.clone(), tier: rank(p).0, link: l })
}

/// The first number in an aisle (`12B` -> 12, `bins 3-4` -> 3).
pub fn aisle_number(aisle: &str) -> Option<f64> {
    let start = aisle.find(|c: char| c.is_ascii_digit())?;
    let rest = &aisle[start..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
    rest[..end].trim_end_matches('.').parse().ok()
}

/// Packs of a product to take for `need` of `unit` (`200 g` of a `16 oz`
/// bag -> `1 x 16 oz`), when its size converts.
pub fn packs(need: f64, unit: &str, p: &Product) -> Option<String> {
    let (q, u) = p.size.as_deref().and_then(matching::size_from_name).or_else(|| matching::size_from_name(&p.name))?;
    let per = matching::convert(q, &u, unit)?;
    if per <= 0.0 || need <= 0.0 {
        return None;
    }
    let n = (need / per - 1e-9).ceil().max(1.0);
    Some(format!("{n} x {} {u}", crate::util::fmt_qty(q)))
}

/// A product as a record (store name and section title included).
pub fn product_json(p: &Product, stores: &Stores) -> Json {
    json!({
        "id": p.id,
        "food": p.food,
        "store": p.store_id,
        "store_name": p.store_id.as_ref().and_then(|s| stores.get(s)).map(|s| s.name.clone()),
        "chain": product_chain(p, stores),
        "name": p.name,
        "brand": p.brand,
        "size": p.size,
        "price": p.price.map(crate::util::money_value),
        "price_seen": p.price_seen,
        "url": p.url,
        "sku": p.sku,
        "plu": p.plu,
        "upc": p.upc,
        "section": p.section,
        "aisle": p.aisle,
        "notes": p.notes,
        "item_id": p.item_id,
    })
}

/// Where a product is, for people: `Sprouts North` (its store) or `Sprouts`
/// (a chain row).
pub fn product_place(p: &Product, stores: &Stores) -> String {
    p.store_id
        .as_ref()
        .and_then(|s| stores.get(s))
        .map(|s| s.name.clone())
        .or_else(|| product_chain(p, stores))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Export and import (the `wherebox/v1` envelope's `stores`, `chain_layouts`
// and `store_products`)
// ---------------------------------------------------------------------------

/// A product as exported: every column, the price in dollars.
pub fn export_product(p: &Product) -> Json {
    let mut v = serde_json::to_value(p).unwrap_or(Json::Null);
    if let Json::Object(m) = &mut v {
        m.remove("food_key");
        m.insert("price".into(), json!(p.price.map(crate::util::money_value)));
    }
    v
}

/// The envelope fields of an export.
pub fn export(db: &Db) -> Result<Vec<(&'static str, Json)>> {
    let layouts: Vec<Json> = chain_layouts(db)?.into_iter().map(|(c, l)| json!({"chain": c, "layout": l})).collect();
    Ok(vec![
        ("stores", json!(all_stores(db)?)),
        ("chain_layouts", json!(layouts)),
        ("store_products", json!(all_products(db)?.iter().map(export_product).collect::<Vec<_>>())),
    ])
}

/// What an import did with stores and products.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Imported {
    pub stores: usize,
    pub chain_layouts: usize,
    pub store_products: usize,
}

fn jtext(v: &Json, k: &str) -> Option<String> {
    match v.get(k) {
        Some(Json::String(s)) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Some(Json::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn natural_key(p: &Product) -> (String, Option<String>, Option<String>, String) {
    (p.food_key.clone(), p.store_id.clone(), p.chain.as_deref().map(chain_key), p.name.to_lowercase())
}

/// Create or update the stores, chain layouts and products of an envelope:
/// stores by id, layouts by chain, products by id when it is the same
/// product, else by (food, store or chain, name), else as new rows.
pub fn import(db: &Db, envelope: &Json) -> Result<Imported> {
    let mut done = Imported::default();
    let now = now_iso();
    for s in envelope.get("stores").and_then(Json::as_array).into_iter().flatten() {
        let id = validate_store_id(&jtext(s, "id").ok_or_else(|| AppError::invalid("a store without an id"))?)?;
        let chain = jtext(s, "chain").ok_or_else(|| AppError::invalid(format!("store {id} has no chain")))?;
        let old = get_store(db, &id)?;
        let st = Store {
            name: jtext(s, "name").unwrap_or_else(|| chain.clone()),
            chain,
            address: jtext(s, "address"),
            website: jtext(s, "website"),
            notes: jtext(s, "notes"),
            layout: jtext(s, "layout").map(|l| Layout::parse(&l).map(|l| l.to_text())).transpose()?,
            created_at: old
                .as_ref()
                .map(|o| o.created_at.clone())
                .or_else(|| jtext(s, "created_at"))
                .unwrap_or_else(|| now.clone()),
            updated_at: jtext(s, "updated_at").unwrap_or_else(|| now.clone()),
            id,
        };
        match old {
            Some(_) => update_store(db, &st)?,
            None => insert_store(db, &st)?,
        }
        done.stores += 1;
    }
    for l in envelope.get("chain_layouts").and_then(Json::as_array).into_iter().flatten() {
        let (Some(chain), Some(layout)) = (jtext(l, "chain"), jtext(l, "layout")) else { continue };
        set_chain_layout(db, &chain, Some(&Layout::parse(&layout)?))?;
        done.chain_layouts += 1;
    }
    let mut existing = all_products(db)?;
    for v in envelope.get("store_products").and_then(Json::as_array).into_iter().flatten() {
        let food = jtext(v, "food").ok_or_else(|| AppError::invalid("a store product without a food"))?;
        let item_id = jtext(v, "item_id").filter(|i| crate::store::get_item(db, i).ok().flatten().is_some());
        let price = match v.get("price") {
            Some(Json::Null) | None => None,
            Some(x) => Some(crate::util::parse_money(&jtext(&json!({"p": x}), "p").unwrap_or_default())?),
        };
        let store_id = jtext(v, "store");
        let mut p = Product {
            id: v.get("id").and_then(Json::as_i64).unwrap_or(0),
            food_key: food_key(&food),
            name: jtext(v, "name").unwrap_or_else(|| food.clone()),
            food,
            item_id,
            chain: if store_id.is_some() { None } else { jtext(v, "chain") },
            store_id,
            brand: jtext(v, "brand"),
            size: jtext(v, "size"),
            price,
            price_seen: jtext(v, "price_seen"),
            url: jtext(v, "url"),
            sku: jtext(v, "sku"),
            plu: jtext(v, "plu"),
            upc: jtext(v, "upc"),
            section: jtext(v, "section").and_then(|s| section_id(&s)).map(|s| s.0),
            aisle: jtext(v, "aisle"),
            notes: jtext(v, "notes"),
            created_at: jtext(v, "created_at").unwrap_or_else(|| now.clone()),
            updated_at: jtext(v, "updated_at").unwrap_or_else(|| now.clone()),
        };
        let key = natural_key(&p);
        let same = existing
            .iter()
            .find(|e| e.id == p.id && natural_key(e) == key)
            .or_else(|| existing.iter().find(|e| natural_key(e) == key))
            .cloned();
        match same {
            Some(e) => {
                p.id = e.id;
                p.created_at = e.created_at;
                update_product(db, &p)?;
                existing.retain(|x| x.id != p.id);
            }
            None => {
                p.id = insert_product(db, &p)?;
            }
        }
        existing.push(p);
        done.store_products += 1;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(
        category: Option<&str>,
        url: Option<&str>,
        name: Option<&str>,
        food: Option<&str>,
    ) -> Option<(String, &'static str)> {
        guess_section(category, url, name, food).map(|g| (g.section, g.basis))
    }

    #[test]
    fn sections_normalize() {
        assert_eq!(section_id("Dry Goods"), Some(("pantry".into(), false)));
        assert_eq!(section_id("pantry/dry goods"), Some(("pantry".into(), false)));
        assert_eq!(section_id("Oils/Vinegar"), Some(("oils-vinegar".into(), false)));
        assert_eq!(section_id("Health & Beauty"), Some(("health-beauty".into(), false)));
        assert_eq!(section_id("vitamins"), Some(("supplements".into(), false)));
        assert_eq!(section_id("center/special-buys"), Some(("special-buys".into(), false)));
        assert_eq!(section_id("Hot Bar"), Some(("hot-bar".into(), true)));
        assert_eq!(section_id("  "), None);
        assert_eq!(section_title("oils-vinegar"), "Oils & vinegar");
        assert_eq!(section_title("hot-bar"), "Hot bar");
    }

    #[test]
    fn section_guess_table() {
        let cases: &[(Option<&str>, Option<&str>, Option<&str>, Option<&str>, &str, &str)] = &[
            (Some("Pantry > Nut Butters"), None, Some("Creamy Almond Butter"), None, "pantry", "category"),
            (Some("Dairy & Eggs"), None, Some("Whole Milk"), None, "dairy", "category"),
            (Some("Frozen > Fruit"), None, None, None, "frozen", "category"),
            (
                None,
                Some("https://shop.example.com/aisles/produce/fresh-fruit/organic-limes?id=4"),
                None,
                None,
                "produce",
                "url",
            ),
            (None, Some("https://shop.example.com/pantry/canned-goods/black-beans"), None, None, "canned", "url"),
            (None, Some("https://shop.example.com/p/12345"), Some("Organic Bananas"), None, "produce", "name"),
            (None, None, Some("Chicken Broth, 32 oz"), None, "pantry", "name"),
            (None, None, Some("Boneless Chicken Breasts"), None, "meat", "name"),
            (None, None, Some("Peanut Butter, Crunchy"), None, "pantry", "name"),
            (None, None, Some("Frozen Wild Blueberries"), None, "frozen", "name"),
            (None, None, Some("Raw Walnut Halves"), None, "pantry", "name"),
            (None, None, Some("Garlic Powder"), None, "spices", "name"),
            (None, None, Some("Extra Virgin Olive Oil"), None, "oils-vinegar", "name"),
            (None, None, Some("Albacore Tuna in Water, 5 oz Can"), None, "canned", "name"),
            (None, None, Some("Greek Yogurt, Plain"), None, "dairy", "name"),
            (None, None, Some("Sourdough Bread"), None, "bakery", "name"),
            (None, None, Some("Large Brown Eggs, 12 ct"), None, "eggs", "name"),
            (None, None, Some("Kettle Chips Sea Salt"), None, "snacks", "name"),
            (None, None, Some("Organic Rolled Oats"), None, "pantry", "name"),
            (None, None, Some("Wild Salmon Fillet"), None, "seafood", "name"),
            (None, None, Some("Quux"), Some("limes"), "produce", "food"),
        ];
        for (c, u, n, f, want, basis) in cases {
            assert_eq!(g(*c, *u, *n, *f), Some(((*want).to_string(), *basis)), "{c:?} {u:?} {n:?} {f:?}");
        }
        assert_eq!(g(None, None, Some("Quux Zorp"), None), None, "no guess");
    }

    #[test]
    fn layouts_parse_and_order() {
        let l = Layout::parse("Produce, bulk=1-2, Dry Goods: 3-7, dairy=11, checkout").unwrap();
        assert_eq!(l.to_text(), "produce, bulk=1-2, pantry=3-7, dairy=11, checkout");
        assert_eq!(l.aisles("pantry"), Some("3-7"));
        assert!(l.order("produce") < l.order("bulk"));
        assert!(l.order("hot-bar") > l.order("dairy"), "not in the layout: before checkout");
        assert!(l.order("hot-bar") < l.order("checkout"));
        assert!(Layout::parse("produce, produce").is_err());
        let d = Layout::default_layout();
        assert_eq!(d.0.first().unwrap().section, "produce");
        assert_eq!(d.0.last().unwrap().section, "checkout");
        assert!(d.order("dairy") > d.order("pantry") && d.order("frozen") > d.order("dairy"));
    }

    #[test]
    fn aisles_and_packs() {
        assert_eq!(aisle_number("12B"), Some(12.0));
        assert_eq!(aisle_number("bins 3-4"), Some(3.0));
        assert_eq!(aisle_number("by the door"), None);
        let p = Product { name: "Walnut Halves".into(), size: Some("16 oz".into()), ..Default::default() };
        assert_eq!(packs(200.0, "g", &p).as_deref(), Some("1 x 16 oz"));
        assert_eq!(packs(1000.0, "g", &p).as_deref(), Some("3 x 16 oz"));
        assert_eq!(packs(2.0, "ea", &p), None);
    }

    #[test]
    fn products_link_by_name_item_and_head_noun() {
        let p = |food: &str| Product { food: food.into(), food_key: food_key(food), ..Default::default() };
        let w = Want { names: vec!["Walnuts".into()], items: vec![] };
        assert_eq!(link(&p("walnuts"), &w), Some(Link::Name));
        assert_eq!(link(&p("walnut halves"), &w), Some(Link::HeadNoun));
        assert_eq!(link(&p("walnut oil"), &w), None);
        let mut linked = p("nuts");
        linked.item_id = Some("ITEM1".into());
        assert_eq!(link(&linked, &Want { items: vec!["ITEM1".into()], ..w.clone() }), Some(Link::Item));
    }
}
