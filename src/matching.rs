//! Pure matching primitives for lists and cohorts (docs/SPEC.md, "Lists and
//! matching"): identifier and GTIN normalization, URL normalization with
//! Amazon ASIN extraction, fuzzy product-name similarity with ranking and
//! ambiguity, and unit conversion.

use std::collections::BTreeSet;

/// Default fuzzy-name threshold (the `match_threshold` setting).
pub const DEFAULT_THRESHOLD: f64 = 0.82;
/// Top-2 candidates closer than this are ambiguous.
pub const AMBIGUITY_GAP: f64 = 0.03;
/// Two tokens count as the same word at or above this Jaro-Winkler similarity.
const TOKEN_SIM: f64 = 0.94;
/// Identifiers shorter than this never match.
const MIN_ID_LEN: usize = 4;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Upper case, letters and digits only (`b0-abc 12` -> `B0ABC12`).
pub fn normalize_id(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

/// GS1 check digit of the digits before it.
fn gtin_check_digit(body: &[u8]) -> u8 {
    let sum: u32 = body.iter().rev().enumerate().map(|(i, d)| u32::from(*d) * if i % 2 == 0 { 3 } else { 1 }).sum();
    ((10 - sum % 10) % 10) as u8
}

/// A UPC / EAN / GTIN as GTIN-14 (left-padded with zeros): 8, 12, 13 or 14
/// digits as is, an 11-digit UPC-A without its check digit gets it computed.
/// Separators (spaces, dashes) are ignored; anything else is not a GTIN.
pub fn normalize_gtin(s: &str) -> Option<String> {
    let t: String = s.chars().filter(|c| !matches!(c, ' ' | '-' | '.')).collect();
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut digits: Vec<u8> = t.bytes().map(|b| b - b'0').collect();
    match digits.len() {
        8 | 12 | 13 | 14 => {}
        11 => digits.push(gtin_check_digit(&digits)),
        _ => return None,
    }
    let mut out = "0".repeat(14 - digits.len());
    out.extend(digits.iter().map(|d| char::from(b'0' + d)));
    Some(out)
}

/// Does a GTIN-14 carry a valid check digit?
pub fn gtin_valid(gtin14: &str) -> bool {
    let d: Vec<u8> = gtin14.bytes().map(|b| b.wrapping_sub(b'0')).collect();
    d.len() == 14 && d.iter().all(|x| *x < 10) && gtin_check_digit(&d[..13]) == d[13]
}

/// Comparison keys of an identifier: its GTIN-14 when it is one, else the
/// normalized id (`None` when too short to be meaningful).
pub fn id_keys(s: &str) -> Vec<String> {
    if let Some(g) = normalize_gtin(s) {
        return vec![format!("GTIN:{g}")];
    }
    let n = normalize_id(s);
    if n.len() < MIN_ID_LEN {
        return Vec::new();
    }
    vec![n]
}

// ---------------------------------------------------------------------------
// URLs
// ---------------------------------------------------------------------------

/// A normalized URL: an Amazon product's ASIN, or host + path (+ kept query).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlKey {
    Asin(String),
    Page(String),
}

const TRACKING: &[&str] = &[
    "ref",
    "ref_",
    "tag",
    "psc",
    "th",
    "gclid",
    "fbclid",
    "_encoding",
    "qid",
    "sr",
    "keywords",
    "crid",
    "sprefix",
    "content-id",
    "smid",
    "linkcode",
    "spm",
    "mc_cid",
    "mc_eid",
];

fn tracking_param(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    TRACKING.contains(&k.as_str())
        || k.starts_with("utm_")
        || k.starts_with("pd_rd_")
        || k.starts_with("pf_rd_")
        || k.starts_with("ref_")
}

fn is_asin(s: &str) -> bool {
    s.len() == 10 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Normalize a URL; `None` when it is not an http(s) URL.
pub fn normalize_url(raw: &str) -> Option<UrlKey> {
    let s = raw.trim();
    let lower = s.to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"))?;
    // Keep the original case of path and query (ASINs, case-sensitive paths).
    let rest_orig = &s[s.len() - rest.len()..];
    let rest_orig = rest_orig.split('#').next().unwrap_or_default();
    let (host_port, tail) = rest_orig.split_once('/').map_or((rest_orig, ""), |(h, t)| (h, t));
    let host_port = host_port.split('@').next_back().unwrap_or_default().to_ascii_lowercase();
    let host = host_port.split(':').next().unwrap_or_default();
    let host = ["www.", "smile.", "m."]
        .iter()
        .fold(host.to_string(), |h, p| h.strip_prefix(p).map_or(h.clone(), str::to_string));
    if host.is_empty() {
        return None;
    }
    let (path, query) = tail.split_once('?').map_or((tail, ""), |(p, q)| (p, q));
    let segs: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if host.starts_with("amazon.") || host.starts_with("amzn.") || host.contains(".amazon.") {
        let lowered: Vec<String> = segs.iter().map(|s| s.to_ascii_lowercase()).collect();
        let after = |pat: &[&str]| -> Option<String> {
            (0..lowered.len()).find_map(|i| {
                let hit = pat.iter().enumerate().all(|(j, p)| lowered.get(i + j).is_some_and(|s| s == p));
                hit.then(|| segs.get(i + pat.len()).map(|s| s.to_ascii_uppercase())).flatten()
            })
        };
        let asin = after(&["dp"])
            .or_else(|| after(&["gp", "product"]))
            .or_else(|| after(&["gp", "aw", "d"]))
            .or_else(|| after(&["o", "asin"]))
            .or_else(|| after(&["exec", "obidos", "asin"]))
            .filter(|a| is_asin(a));
        if let Some(a) = asin {
            return Some(UrlKey::Asin(a));
        }
    }
    let mut kept: Vec<&str> = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .filter(|kv| !tracking_param(kv.split('=').next().unwrap_or_default()))
        .collect();
    kept.sort_unstable();
    let mut out = format!("{host}/{}", segs.join("/"));
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    Some(UrlKey::Page(out.trim_end_matches('/').to_string()))
}

/// Every http(s) URL in free text.
pub fn urls_in(text: &str) -> Vec<&str> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
        .map(|w| w.trim_start_matches(['(', '[']).trim_end_matches([')', ']', ',', '.', ';']))
        .filter(|w| w.starts_with("http://") || w.starts_with("https://"))
        .collect()
}

/// A display name from a URL: the Amazon title slug before `/dp/`, else the
/// last path segment, else the host.
pub fn name_from_url(raw: &str) -> String {
    let key = normalize_url(raw);
    let s = raw.trim();
    let path = s.split_once("://").map_or(s, |(_, r)| r).split(['?', '#']).next().unwrap_or_default();
    let segs: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let slug = |seg: &str| seg.replace(['-', '_', '+'], " ").trim().to_string();
    if let Some(UrlKey::Asin(a)) = &key {
        if let Some(i) = segs.iter().position(|s| s.eq_ignore_ascii_case("dp")) {
            if i >= 2 {
                return slug(segs[i - 1]);
            }
        }
        return format!("amazon {a}");
    }
    segs.iter()
        .skip(1)
        .rev()
        .map(|s| slug(s.split('.').next().unwrap_or(s)))
        .find(|s| !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or_else(|| segs.first().map_or_else(String::new, |h| h.to_string()))
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// Words that say nothing about what the product is.
const STOPWORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "of",
    "and",
    "or",
    "with",
    "for",
    "by",
    "in",
    "to",
    "from",
    "premium",
    "quality",
    "value",
    "family",
    "size",
    "bulk",
    "new",
    "brand",
    "original",
    "classic",
    "great",
    "best",
    "pure",
    "natural",
    "naturally",
    "fresh",
    "freshly",
    "deluxe",
    "gourmet",
    "select",
    "choice",
    "authentic",
    "genuine",
    "amazon",
    "pack",
    "packs",
    "pk",
    "count",
    "ct",
    "pcs",
    "pc",
    "piece",
    "pieces",
    "approx",
    "about",
    "chopped",
    "diced",
    "minced",
    "sliced",
    "crushed",
    "softened",
    "melted",
    "toasted",
    "roasted",
    "grated",
    "shredded",
    "large",
    "small",
    "medium",
    "extra",
    "x",
    "oz",
    "ounce",
    "ounces",
    "fl",
    "lb",
    "lbs",
    "pound",
    "pounds",
    "g",
    "gram",
    "grams",
    "kg",
    "ml",
    "l",
    "liter",
    "liters",
    "litre",
    "litres",
    "gal",
    "gallon",
    "qt",
    "quart",
];

/// Size units, for tokens such as `5oz`, `12ct`, `2l`.
const SIZE_UNITS: &[&str] = &[
    "oz", "floz", "lb", "lbs", "g", "gr", "kg", "mg", "ml", "l", "cl", "ct", "count", "pk", "pack", "pcs", "pc", "x",
    "in", "inch", "ft", "mm", "cm", "gal", "qt",
];

fn is_number(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.') && t.chars().any(|c| c.is_ascii_digit())
}

/// `5oz`, `12pk`, `x3`, `3x`, `1.5l`.
fn is_size_token(t: &str) -> bool {
    if let Some(n) = t.strip_prefix('x') {
        if is_number(n) {
            return true;
        }
    }
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    is_number(num) && (unit.is_empty() || SIZE_UNITS.contains(&unit))
}

/// A word's singular (`berries` -> `berry`, `tomatoes` -> `tomato`).
pub fn singular(t: &str) -> String {
    if t.len() <= 3 || t.ends_with("ss") || t.ends_with("us") || t.ends_with("is") {
        return t.to_string();
    }
    if let Some(s) = t.strip_suffix("ies") {
        return format!("{s}y");
    }
    for suf in ["oes", "ches", "shes", "xes", "sses", "zes"] {
        if let Some(s) = t.strip_suffix(suf) {
            return format!("{s}{}", &suf[..suf.len() - 2]);
        }
    }
    t.strip_suffix('s').map_or_else(|| t.to_string(), str::to_string)
}

/// Normalized name tokens: lower case, punctuation split, pack/size tokens,
/// stopwords and bare numbers dropped, singular.
pub fn name_tokens(s: &str) -> Vec<String> {
    let lower = s.to_lowercase();
    // Keep decimal points between digits ("1.5l"); everything else that is
    // not a letter or digit separates words.
    let chars: Vec<char> = lower.chars().collect();
    let cleaned: String = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let digit_at = |j: Option<usize>| j.and_then(|j| chars.get(j)).is_some_and(char::is_ascii_digit);
            if c.is_alphanumeric() || (c == '.' && digit_at(i.checked_sub(1)) && digit_at(Some(i + 1))) {
                c
            } else {
                ' '
            }
        })
        .collect();
    let mut out = Vec::new();
    for t in cleaned.split_whitespace() {
        if is_number(t) || is_size_token(t) || STOPWORDS.contains(&t) {
            continue;
        }
        let s = singular(t);
        if !STOPWORDS.contains(&s.as_str()) && !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// The normalized name as one string (tokens joined by spaces).
pub fn normalize_name(s: &str) -> String {
    name_tokens(s).join(" ")
}

/// Similarity of a query to a candidate name, and whether every query token
/// was found in the candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NameScore {
    pub score: f64,
    pub full: bool,
}

/// Token-set similarity of pre-tokenized names (see [`name_tokens`]).
pub fn score_tokens(q: &[String], c: &[String]) -> NameScore {
    if q.is_empty() || c.is_empty() {
        return NameScore { score: 0.0, full: false };
    }
    let mut used = vec![false; c.len()];
    let mut matched = 0.0;
    let mut full = true;
    for qt in q {
        let best = c
            .iter()
            .enumerate()
            .filter(|(i, _)| !used[*i])
            .map(|(i, ct)| (i, if qt == ct { 1.0 } else { strsim::jaro_winkler(qt, ct) }))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((i, s)) if s >= TOKEN_SIM => {
                used[i] = true;
                matched += s;
            }
            _ => full = false,
        }
    }
    let score = 0.9 * matched / q.len() as f64 + 0.1 * matched / c.len() as f64;
    NameScore { score: (score * 1e4).round() / 1e4, full }
}

/// Similarity of two raw names.
pub fn score_names(query: &str, candidate: &str) -> NameScore {
    score_tokens(&name_tokens(query), &name_tokens(candidate))
}

/// One ranked fuzzy candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked<K> {
    pub key: K,
    pub name: String,
    pub score: NameScore,
}

/// Candidates at or above `threshold`, best first (ties by name), and whether
/// the result is ambiguous: the best two distinct names (by normalized name)
/// are within [`AMBIGUITY_GAP`] and do not both contain every query token.
pub fn rank<K: Clone>(query: &str, candidates: &[(K, String)], threshold: f64) -> (Vec<Ranked<K>>, bool) {
    let q = name_tokens(query);
    let mut out: Vec<Ranked<K>> = candidates
        .iter()
        .map(|(k, n)| Ranked { key: k.clone(), name: n.clone(), score: score_tokens(&q, &name_tokens(n)) })
        .filter(|r| r.score.score >= threshold)
        .collect();
    out.sort_by(|a, b| b.score.score.total_cmp(&a.score.score).then_with(|| a.name.cmp(&b.name)));
    (out.clone(), ambiguous(&out))
}

/// See [`rank`].
pub fn ambiguous<K>(ranked: &[Ranked<K>]) -> bool {
    let mut seen = BTreeSet::new();
    let distinct: Vec<&Ranked<K>> = ranked.iter().filter(|r| seen.insert(normalize_name(&r.name))).take(2).collect();
    match distinct.as_slice() {
        [a, b] => a.score.score - b.score.score <= AMBIGUITY_GAP + 1e-9 && !(a.score.full && b.score.full),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Head nouns
// ---------------------------------------------------------------------------

/// Words that describe a product without naming it: skipped when looking for
/// the head noun (the last word that does name it).
const MODIFIERS: &[&str] = &[
    "organic",
    "whole",
    "raw",
    "frozen",
    "wild",
    "baby",
    "mini",
    "ripe",
    "salted",
    "unsalted",
    "plain",
    "sweetened",
    "unsweetened",
    "boneless",
    "skinless",
    "grass",
    "fed",
    "pasture",
    "raised",
    "free",
    "range",
    "cage",
    "lean",
    "local",
    "imported",
    "seedless",
    "peeled",
    "hulled",
    "shelled",
    "cleaned",
    "trimmed",
    "washed",
    "dried",
    "usda",
    "non",
    "gmo",
    "certified",
    "kosher",
    "vegan",
    "gluten",
    "conventional",
    "assorted",
    "mixed",
    "variety",
];

/// Containers and portions: skipped when looking for the head noun
/// (`Garlic, 1 Head`, `Bananas 3 lb Bag`).
const PACKAGING: &[&str] = &[
    "bag",
    "box",
    "bunch",
    "head",
    "bulb",
    "jar",
    "can",
    "bottle",
    "carton",
    "container",
    "pouch",
    "tub",
    "tray",
    "case",
    "package",
    "pkg",
    "sleeve",
    "loaf",
    "clamshell",
    "net",
    "sack",
    "crate",
    "dozen",
    "pint",
    "ea",
    "each",
    "unit",
    "serving",
    "servings",
];

/// Cuts and forms: skipped when looking for the head noun (`Walnut Halves`
/// are walnuts, `Ginger Root` is ginger, `Garlic Cloves` are garlic).
const FORMS: &[&str] = &[
    "halve", "half", "slice", "chunk", "bit", "flake", "floret", "wedge", "strip", "fillet", "filet", "spear", "cube",
    "crumble", "kernel", "root", "leaf", "leave", "clove", "stalk", "sprig", "stem",
];

/// Food forms: what a product is when its name has one, wherever it sits
/// (`Huy Fong Sauce Chili Garlic` is a sauce, `Chex Cereal, Blueberry` a
/// cereal). Another food in the name is only its flavour.
const FOOD_FORMS: &[&str] = &[
    "sauce",
    "paste",
    "cereal",
    "mix",
    "chip",
    "crisp",
    "bar",
    "powder",
    "juice",
    "oil",
    "tea",
    "candy",
    "gummy",
    "seasoning",
    "dressing",
    "vinaigrette",
    "marinade",
    "rub",
    "spread",
    "jam",
    "jelly",
    "preserve",
    "syrup",
    "extract",
    "flavoring",
    "cracker",
    "cookie",
    "drink",
    "soda",
    "smoothie",
    "ketchup",
    "mayo",
    "mayonnaise",
    "salsa",
    "broth",
    "soup",
    "granola",
];

fn is_form(t: &str) -> bool {
    FOOD_FORMS.contains(&t)
}

fn skipped(t: &str) -> bool {
    MODIFIERS.contains(&t) || PACKAGING.contains(&t) || FORMS.contains(&t)
}

/// Words that end the part of a product name that says what it is
/// (`Tuna in Water`, `Tortillas with Flax`, `Snacks for Kids`).
const SEGMENT_WORDS: &[&str] = &["in", "with", "for", "from", "by", "w"];

/// One part of a product name (split at commas, parentheses, dashes and
/// `in`/`with`/`for`...): its tokens and its head noun.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub tokens: Vec<String>,
    /// Tokens that name the product (modifiers and packaging dropped).
    pub core: Vec<String>,
    pub head: Option<String>,
    /// The segment follows `in`/`with`/`for`... (`Tuna in Olive Oil`): it
    /// does not say what the product is.
    pub trailing: bool,
}

/// The segments of a product name, for head-noun matching.
pub fn segments(name: &str) -> Vec<Segment> {
    let lower = name.to_lowercase().replace(" - ", ",").replace(" – ", ",");
    let mut parts: Vec<(String, bool)> = Vec::new();
    for chunk in lower.split([',', ';', ':', '|', '(', ')', '[', ']', '/', '+']) {
        let mut cur = String::new();
        let mut trailing = false;
        for w in chunk.split_whitespace() {
            let bare = w.trim_matches(|c: char| !c.is_alphanumeric());
            if SEGMENT_WORDS.contains(&bare) && !cur.trim().is_empty() {
                parts.push((std::mem::take(&mut cur), trailing));
                trailing = true;
                continue;
            }
            cur.push(' ');
            cur.push_str(w);
        }
        parts.push((cur, trailing));
    }
    parts
        .iter()
        .map(|(p, trailing)| {
            let tokens = name_tokens(p);
            let core: Vec<String> = tokens.iter().filter(|t| !skipped(t)).cloned().collect();
            // a food form names the product wherever it is; else the last
            // word that is not skipped; a name made only of skipped words
            // (`Whole Cloves`) is its own head
            let head = core.iter().rev().find(|t| is_form(t)).or(core.last()).or(tokens.last()).cloned();
            Segment { tokens, core, head, trailing: *trailing }
        })
        .filter(|s| !s.tokens.is_empty())
        .collect()
}

/// The same word, allowing a typo in longer words (`wallnut`), but not
/// telling short ones apart by one letter (`drill` is not `dill`).
fn same_word(a: &str, b: &str) -> bool {
    a == b || (a.len().min(b.len()) >= 6 && strsim::jaro_winkler(a, b) >= TOKEN_SIM)
}

/// The head noun of a phrase (`chicken breasts` -> `breast`, `garlic
/// powder` -> `powder`).
pub fn phrase_head(tokens: &[String]) -> Option<&String> {
    tokens.iter().rev().find(|t| is_form(t)).or_else(|| tokens.iter().rev().find(|t| !skipped(t))).or(tokens.last())
}

/// The food forms a product name says it is: the heads of its segments
/// that are forms, outside `in`/`with`... trailers (`Chex Cereal,
/// Blueberry` -> `cereal`).
pub fn forms(segs: &[Segment]) -> Vec<&str> {
    segs.iter().filter(|s| !s.trailing).filter_map(|s| s.head.as_deref()).filter(|h| is_form(h)).collect()
}

/// How a phrase (a shelf-life type, a recipe entry) names a product.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadMatch {
    /// 0.85-1.0 when the phrase's head noun is the head noun of a segment of
    /// the name (and its other words are in that segment, more of the
    /// segment named scoring higher); 0.6 when only the head noun is; 0.5
    /// when it only appears as a modifier (`garlic` in `Chili Garlic
    /// Sauce`), or the product is a food form the phrase is not (`blueberry`
    /// in `Chex Cereal, Blueberry`); else 0.
    pub score: f64,
    /// The head noun is there, but not as the head: a compound product.
    pub compound: bool,
}

/// Score `phrase` (normalized tokens) against a product name's segments.
pub fn head_match(phrase: &[String], segs: &[Segment]) -> HeadMatch {
    let none = HeadMatch { score: 0.0, compound: false };
    let Some(ph) = phrase_head(phrase) else { return none };
    let product = forms(segs);
    if !product.is_empty() && !product.iter().any(|f| same_word(f, ph)) {
        // a flavour of a sauce, cereal, bar...: at most a modifier
        let there = segs.iter().any(|s| s.tokens.iter().any(|x| same_word(x, ph)));
        return if there { HeadMatch { score: 0.5, compound: true } } else { none };
    }
    let mut best = none;
    for s in segs {
        let others = phrase.iter().filter(|t| *t != ph);
        let all_there = others.clone().all(|t| s.tokens.iter().any(|x| same_word(t, x)));
        let is_head = s.head.as_deref().is_some_and(|h| same_word(h, ph));
        if is_head && all_there {
            let named = phrase.iter().filter(|t| s.core.iter().any(|x| same_word(t, x))).count();
            let cover = named as f64 / s.core.len().max(1) as f64;
            let score = ((0.85 + 0.15 * cover.min(1.0)) * 1e4).round() / 1e4;
            if score > best.score {
                best = HeadMatch { score, compound: false };
            }
        } else if is_head && best.score < 0.6 {
            // the head noun, but not every other word of the phrase
            best = HeadMatch { score: 0.6, compound: false };
        } else if s.tokens.iter().any(|x| same_word(x, ph)) && best.score < 0.5 {
            best = HeadMatch { score: 0.5, compound: true };
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dim {
    Mass,
    Volume,
    Count,
}

/// Known units: canonical spelling, dimension, size in the base unit (g,
/// ml, ea), and the other spellings accepted.
const UNITS: &[(&str, Dim, f64, &[&str])] = &[
    ("g", Dim::Mass, 1.0, &["gr", "gram", "grams", "gramme", "grammes"]),
    ("mg", Dim::Mass, 0.001, &["milligram", "milligrams"]),
    ("kg", Dim::Mass, 1000.0, &["kgs", "kilo", "kilos", "kilogram", "kilograms"]),
    ("oz", Dim::Mass, 28.349_523_125, &["ounce", "ounces"]),
    ("lb", Dim::Mass, 453.592_37, &["lbs", "pound", "pounds"]),
    ("ml", Dim::Volume, 1.0, &["milliliter", "milliliters", "millilitre", "millilitres"]),
    ("cl", Dim::Volume, 10.0, &[]),
    ("l", Dim::Volume, 1000.0, &["liter", "liters", "litre", "litres"]),
    ("fl oz", Dim::Volume, 29.573_529_6, &["floz", "fluid ounce", "fluid ounces"]),
    ("tsp", Dim::Volume, 4.928_921_6, &["teaspoon", "teaspoons"]),
    ("tbsp", Dim::Volume, 14.786_764_8, &["tablespoon", "tablespoons"]),
    ("cup", Dim::Volume, 236.588_236_5, &["cups"]),
    ("gal", Dim::Volume, 3_785.411_784, &["gallon", "gallons"]),
    ("ea", Dim::Count, 1.0, &["", "each", "ct", "count", "pc", "pcs", "piece", "pieces"]),
    // count units: interchangeable with `ea`, one for one
    ("can", Dim::Count, 1.0, &["cans"]),
    ("jar", Dim::Count, 1.0, &["jars"]),
    ("bottle", Dim::Count, 1.0, &["bottles"]),
    ("box", Dim::Count, 1.0, &["boxes"]),
    ("bag", Dim::Count, 1.0, &["bags"]),
    ("pack", Dim::Count, 1.0, &["packs", "pk"]),
    ("packet", Dim::Count, 1.0, &["packets"]),
    ("tube", Dim::Count, 1.0, &["tubes"]),
    ("capsule", Dim::Count, 1.0, &["capsules"]),
];

fn unit_entry(u: &str) -> Option<&'static (&'static str, Dim, f64, &'static [&'static str])> {
    let u = u.trim().to_ascii_lowercase().replace('.', "");
    UNITS.iter().find(|(c, _, _, alt)| *c == u || alt.contains(&u.as_str()))
}

/// A unit's dimension and size in the base unit (g, ml, ea), for known units.
pub fn unit_info(u: &str) -> Option<(Dim, f64)> {
    unit_entry(u).map(|(_, d, f, _)| (*d, *f))
}

/// The canonical spelling of a unit word (`grams` -> `g`); unknown units are
/// kept (lower case).
pub fn canonical_unit(u: &str) -> String {
    unit_entry(u).map_or_else(|| u.trim().to_ascii_lowercase(), |(c, ..)| c.to_string())
}

/// `qty` of `from` in `to`, when the units are compatible (unknown units
/// convert only to themselves).
pub fn convert(qty: f64, from: &str, to: &str) -> Option<f64> {
    let (f, t) = (from.trim().to_ascii_lowercase(), to.trim().to_ascii_lowercase());
    match (unit_info(&f), unit_info(&t)) {
        (Some((df, sf)), Some((dt, st))) if df == dt => Some(qty * sf / st),
        (None, None) if f == t => Some(qty),
        _ => None,
    }
}

/// The number of units in a pack a product name states (`Tuna, 12-Pack`,
/// `Pack of 12`, `12pk`, `90 Count`, `12ct`): the last pack phrase times the
/// last count phrase (`24 Count (Pack of 4)` -> 96).
pub fn pack_count_from_name(name: &str) -> Option<f64> {
    let lower = name.to_lowercase();
    let toks: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | '/' | '-' | '[' | ']' | ';'))
        .filter(|t| !t.is_empty())
        .collect();
    let num = |t: &str| t.parse::<f64>().ok().filter(|n| *n >= 1.0 && n.fract() == 0.0);
    let (mut pack, mut count) = (None, None);
    for (i, t) in toks.iter().enumerate() {
        let next = toks.get(i + 1).copied().unwrap_or("");
        if *t == "pack" && next == "of" {
            if let Some(n) = toks.get(i + 2).and_then(|t| num(t)) {
                pack = Some(n);
            }
            continue;
        }
        let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
        let Some(n) = num(&t[..split]) else { continue };
        let word = if split == t.len() { next } else { &t[split..] };
        match word {
            "pack" | "pk" => pack = Some(n),
            "count" | "ct" => count = Some(n),
            _ => {}
        }
    }
    match (pack, count) {
        (None, None) => None,
        (p, c) => Some(p.unwrap_or(1.0) * c.unwrap_or(1.0)),
    }
}

/// The package size a product name states (`Walnut Halves, 16 oz` ->
/// `(16, "oz")`, `Olive Oil 1.5L` -> `(1.5, "l")`): the last mass or volume
/// quantity in the name.
pub fn size_from_name(name: &str) -> Option<(f64, String)> {
    let lower = name.to_lowercase().replace("fl. oz", "floz").replace("fl oz", "floz").replace("fluid ounces", "floz");
    let toks: Vec<String> = lower
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | '/' | '-' | '[' | ']'))
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    let measure = |n: &str, u: &str| -> Option<(f64, String)> {
        let q: f64 = n.parse().ok()?;
        let u = u.trim_end_matches('.');
        matches!(unit_info(u), Some((Dim::Mass | Dim::Volume, _))).then(|| (q, canonical_unit(u)))
    };
    let mut found = None;
    for (i, t) in toks.iter().enumerate() {
        let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
        let (num, unit) = t.split_at(split);
        if num.is_empty() {
            continue;
        }
        let hit = if unit.is_empty() { toks.get(i + 1).and_then(|u| measure(num, u)) } else { measure(num, unit) };
        if hit.is_some() {
            found = hit;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_normalize_case_and_separators() {
        assert_eq!(normalize_id(" b0-abc 12 "), "B0ABC12");
        assert_eq!(id_keys("ab"), Vec::<String>::new());
        assert_eq!(id_keys("sku-12ab"), ["SKU12AB"]);
    }

    #[test]
    fn upc_and_ean_compare_as_gtin14() {
        // Coca-Cola 12 oz can: UPC-A 049000028911
        let upc = normalize_gtin("0 49000 02891 1").unwrap();
        assert_eq!(upc, "00049000028911");
        assert!(gtin_valid(&upc));
        assert_eq!(normalize_gtin("0049000028911"), Some(upc.clone()), "EAN-13 with a leading zero");
        assert_eq!(normalize_gtin("04900002891"), Some(upc.clone()), "11 digits: check digit computed");
        assert_eq!(
            normalize_gtin("49000028911"),
            Some("00490000289114".into()),
            "leading zero missing: a different code"
        );
        assert_eq!(normalize_gtin("96385074"), Some("00000096385074".into()), "EAN-8");
        assert!(gtin_valid("00000096385074"));
        assert!(!gtin_valid("00049000028912"));
        assert_eq!(normalize_gtin("B0ABCDEF12"), None);
        assert_eq!(normalize_gtin("12345"), None);
        assert_eq!(id_keys("049000028911"), id_keys("0049000028911"));
    }

    #[test]
    fn amazon_urls_yield_the_asin() {
        let a = |s: &str| normalize_url(s);
        let want = Some(UrlKey::Asin("B07XJ8C8F5".into()));
        assert_eq!(a("https://www.amazon.com/Diamond-Walnuts/dp/B07XJ8C8F5/ref=sr_1_3?keywords=walnuts&qid=1"), want);
        assert_eq!(a("https://amazon.com/gp/product/b07xj8c8f5?psc=1"), want);
        assert_eq!(a("http://smile.amazon.co.uk/gp/aw/d/B07XJ8C8F5"), want);
        assert_eq!(a("https://www.amazon.de/exec/obidos/ASIN/B07XJ8C8F5"), want);
        assert_eq!(a("https://amazon.com/o/ASIN/B07XJ8C8F5/"), want);
        assert_eq!(a("https://www.amazon.com/s?k=walnuts"), Some(UrlKey::Page("amazon.com/s?k=walnuts".into())));
        assert_eq!(a("ftp://example.com"), None);
        assert_eq!(a("not a url"), None);
    }

    #[test]
    fn other_urls_compare_host_and_path() {
        let p = |s: &str| normalize_url(s);
        assert_eq!(
            p("https://www.REI.com/product/12345/tent/?utm_source=x&color=red&ref_=abc#reviews"),
            Some(UrlKey::Page("rei.com/product/12345/tent?color=red".into()))
        );
        assert_eq!(
            p("http://rei.com/product/12345/tent?color=red"),
            p("https://www.rei.com/product/12345/tent/?color=red")
        );
        assert_ne!(p("https://rei.com/a"), p("https://rei.com/b"));
        assert_eq!(urls_in("see (https://x.com/a), and http://y.org."), ["https://x.com/a", "http://y.org"]);
        assert_eq!(name_from_url("https://www.amazon.com/Diamond-Walnuts/dp/B07XJ8C8F5/ref=x"), "Diamond Walnuts");
        assert_eq!(name_from_url("https://amazon.com/dp/B07XJ8C8F5"), "amazon B07XJ8C8F5");
        assert_eq!(name_from_url("https://www.rei.com/product/12345/half-dome-tent"), "half dome tent");
    }

    #[test]
    fn names_drop_sizes_stopwords_and_plurals() {
        assert_eq!(
            normalize_name("Diamond of California Shelled Walnut Halves & Pieces, 16 oz (Pack of 2)"),
            "diamond california shelled walnut halve"
        );
        assert_eq!(normalize_name("Organic Blueberries, 12-pack 5oz"), "organic blueberry");
        assert_eq!(normalize_name("Tomatoes x3 1.5lb"), "tomato");
        assert_eq!(normalize_name("Large eggs, 18 ct"), "egg");
        assert_eq!(normalize_name("Swiss cheese"), "swiss cheese");
        assert_eq!(normalize_name("Peaches"), "peach");
    }

    #[test]
    fn fuzzy_scores() {
        assert_eq!(score_names("walnuts", "Walnut").score, 1.0);
        let long = score_names("walnuts", "Diamond of California Shelled Walnut Halves & Pieces, 16 oz");
        assert!(long.full && long.score >= 0.9 && long.score < 1.0, "{long:?}");
        let typo = score_names("wallnuts", "walnuts");
        assert!(typo.score > 0.95, "{typo:?}");
        assert!(score_names("butter", "buttermilk").score < 0.5);
        assert!(score_names("walnut oil", "walnut halves").score < DEFAULT_THRESHOLD);
        assert!(score_names("pecans", "Pecorino Romano").score < 0.5);
        assert_eq!(score_names("", "x").score, 0.0);
    }

    #[test]
    fn ranking_and_ambiguity() {
        let c = |v: &[&str]| v.iter().enumerate().map(|(i, s)| (i, s.to_string())).collect::<Vec<_>>();
        let (r, amb) = rank("walnuts", &c(&["Pecan halves", "Walnut halves 1 lb", "Walnuts", "Walnut pieces"]), 0.82);
        assert_eq!(r.iter().map(|x| x.key).collect::<Vec<_>>(), [3, 2, 1], "ties by name");
        assert!(!amb, "all walnuts: counted together, not ambiguous");
        // The same product twice (repeat purchases) is never ambiguous with itself.
        let (_, amb) = rank("almond milk", &c(&["Almond milk", "Almond Milk, 64 fl oz", "Oat milk"]), 0.82);
        assert!(!amb);
        // Partial matches close together: which one?
        let (r, amb) = rank("dark chocolate chips", &c(&["Dark chocolate bar", "Dark chocolate chunks"]), 0.5);
        assert_eq!(r.len(), 2);
        assert!(amb, "{r:?}");
        let (r, amb) = rank("dark chocolate chips", &c(&["Dark chocolate chips", "Chocolate chips"]), 0.5);
        assert!(!amb && r[0].key == 0, "{r:?}");
        assert!(rank("caviar", &c(&["Walnuts"]), 0.82).0.is_empty());
    }

    #[test]
    fn units_convert_within_a_dimension() {
        let close = |a: Option<f64>, b: f64| (a.unwrap() - b).abs() < 1e-6;
        assert!(close(convert(1.0, "lb", "g"), 453.59237));
        assert!(close(convert(16.0, "oz", "lb"), 1.0));
        assert!(close(convert(250.0, "ml", "l"), 0.25));
        assert!(close(convert(2.0, "kg", "grams"), 2000.0));
        assert!(close(convert(3.0, "ea", "count"), 3.0));
        assert!(close(convert(2.0, "clove", "clove"), 2.0));
        assert_eq!(convert(1.0, "g", "ml"), None);
        assert_eq!(convert(1.0, "ea", "g"), None);
        assert_eq!(convert(1.0, "g", "can"), None);
        assert_eq!(canonical_unit("Grams"), "g");
        assert_eq!(canonical_unit("tablespoons"), "tbsp");
        assert_eq!(canonical_unit("clove"), "clove");
    }

    #[test]
    fn count_units_are_one_dimension() {
        for u in ["can", "cans", "jar", "bottle", "box", "bag", "pack", "packet", "piece", "pcs", "tube", "count"] {
            assert_eq!(convert(2.0, u, "ea"), Some(2.0), "{u}");
        }
        assert_eq!(convert(1.0, "can", "jar"), Some(1.0));
        assert_eq!(canonical_unit("Cans"), "can");
        assert_eq!(canonical_unit("pcs"), "ea");
    }

    #[test]
    fn pack_counts_from_product_names() {
        assert_eq!(pack_count_from_name("StarKist Albacore Tuna in Water, 5 oz Can, 12-Pack"), Some(12.0));
        assert_eq!(pack_count_from_name("Albacore Tuna (Pack of 12)"), Some(12.0));
        assert_eq!(pack_count_from_name("Albacore Tuna 12 pack"), Some(12.0));
        assert_eq!(pack_count_from_name("Batteries 12ct"), Some(12.0));
        assert_eq!(pack_count_from_name("Fish Oil, 90 Count"), Some(90.0));
        assert_eq!(pack_count_from_name("Coffee Pods, 24 Count (Pack of 4)"), Some(96.0));
        assert_eq!(pack_count_from_name("Walnut Halves, 16 oz"), None);
        assert_eq!(pack_count_from_name("Six-pack abs roller"), None);
    }

    #[test]
    fn sizes_from_product_names() {
        assert_eq!(size_from_name("Walnut Halves & Pieces, 16 oz (Pack of 2)"), Some((16.0, "oz".into())));
        assert_eq!(size_from_name("Olive Oil 1.5L"), Some((1.5, "l".into())));
        assert_eq!(size_from_name("Oat milk, 64 fl oz"), Some((64.0, "fl oz".into())));
        assert_eq!(size_from_name("Maple syrup 500ml bottle"), Some((500.0, "ml".into())));
        assert_eq!(size_from_name("AA batteries, 24 count"), None);
    }

    #[test]
    fn food_forms_are_the_head_wherever_they_sit() {
        let hm = |phrase: &str, name: &str| head_match(&name_tokens(phrase), &segments(name)).score;
        // garlic only flavours a sauce, even when the brand puts "Sauce" first
        assert_eq!(segments("Huy Fong Sauce Chili Garlic 18oz")[0].head.as_deref(), Some("sauce"));
        assert_eq!(hm("garlic", "Huy Fong Sauce Chili Garlic 18oz"), 0.5);
        assert_eq!(hm("garlic", "Tuong Ot Toi Viet-Nam Chili Garlic Sauce"), 0.5);
        assert!(hm("chili sauce", "Huy Fong Sauce Chili Garlic 18oz") >= 0.85);
        // a flavour in its own segment of a cereal is still a flavour
        assert_eq!(forms(&segments("Chex Cereal, Blueberry, Gluten Free, 12 oz")), ["cereal"]);
        assert_eq!(hm("blueberries", "Chex Cereal, Blueberry, Gluten Free, 12 oz"), 0.5);
        // a form the phrase names is the product
        assert!(hm("garlic powder", "Garlic Powder") >= 0.85);
        assert_eq!(hm("garlic", "Garlic Powder"), 0.5);
        // `in`/`with` trailers do not say what the product is
        assert!(hm("tuna", "Tuna in Olive Oil, 5 oz") >= 0.85);
        // no form: the head noun as before
        for (p, n) in [
            ("garlic", "Organic Garlic, 1 Head"),
            ("garlic", "Fresh Garlic 3 lb bag"),
            ("blueberries", "Wyman's Frozen Wild Blueberries 3 lb"),
            ("raspberries", "Organic Raspberries 6 oz"),
        ] {
            assert!(hm(p, n) >= 0.85, "{p} in {n}");
        }
    }
}
