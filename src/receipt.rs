//! Receipt parsing (docs/SPEC.md, "Receipts"): the text of a grocery
//! receipt (OCR output or an emailed receipt) into item lines with codes,
//! quantities, weights, discounts and voids, plus the store header, date,
//! time and totals.
//!
//! One generic line parser, tuned per chain by profiles that are data
//! (`data/receipts.toml`): header phrases that detect the chain, the
//! lengths of its own item codes, its date order, its final-price, discount
//! and fee words, and its abbreviations.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The built-in receipt data.
pub const DATA: &str = include_str!("../data/receipts.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    #[serde(default)]
    pub fee_words: Vec<String>,
    #[serde(default)]
    pub discount_words: Vec<String>,
    #[serde(default)]
    pub abbreviations: BTreeMap<String, String>,
    #[serde(default)]
    pub chain: Vec<ChainProfile>,
}

/// How one chain's receipts read.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainProfile {
    pub name: String,
    #[serde(default)]
    pub detect: Vec<String>,
    #[serde(default)]
    pub sku_digits: Vec<usize>,
    #[serde(default = "mdy")]
    pub date_order: String,
    #[serde(default)]
    pub final_price_words: Vec<String>,
    #[serde(default)]
    pub discount_words: Vec<String>,
    #[serde(default)]
    pub fee_words: Vec<String>,
    #[serde(default)]
    pub abbreviations: BTreeMap<String, String>,
}

fn mdy() -> String {
    "mdy".into()
}

impl ChainProfile {
    pub fn is_generic(&self) -> bool {
        self.name == "generic"
    }
}

/// The parsed data file (checked by a test, so it always loads).
pub fn data() -> &'static Data {
    static D: OnceLock<Data> = OnceLock::new();
    D.get_or_init(|| toml::from_str(DATA).expect("data/receipts.toml parses"))
}

/// The chain profile named `name` (case-insensitive).
pub fn profile(name: &str) -> Option<&'static ChainProfile> {
    data().chain.iter().find(|p| p.name.eq_ignore_ascii_case(name.trim()))
}

/// The fallback profile.
pub fn generic() -> &'static ChainProfile {
    profile("generic").expect("data/receipts.toml has a generic profile")
}

/// Lower-case alphanumeric words (`Sprouts Farmers Market #12` ->
/// `sprouts farmers market 12`).
pub fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| w.trim_matches('\''))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Does `phrase` occur in `ws` as whole consecutive words?
fn has_phrase(ws: &[String], phrase: &str) -> bool {
    let p = words(phrase);
    !p.is_empty() && ws.windows(p.len()).any(|w| w == p.as_slice())
}

fn any_phrase<S: AsRef<str>>(ws: &[String], phrases: &[S]) -> bool {
    phrases.iter().any(|p| has_phrase(ws, p.as_ref()))
}

/// The chain whose `detect` phrase is in the header (longest phrase wins).
pub fn detect_chain<S: AsRef<str>>(header: &[S]) -> Option<&'static ChainProfile> {
    let ws: Vec<String> = header.iter().flat_map(|l| words(l.as_ref())).collect();
    data()
        .chain
        .iter()
        .flat_map(|p| p.detect.iter().map(move |d| (p, d)))
        .filter(|(_, d)| has_phrase(&ws, d))
        .max_by_key(|(_, d)| words(d).len())
        .map(|(p, _)| p)
}

// ---------------------------------------------------------------------------
// Amounts, codes, sizes and names
// ---------------------------------------------------------------------------

/// An amount in cents: `1.99`, `$1.99`, `-1.99`, `1.99-`, `(1.99)`, `-$1.99`,
/// `1,99` (two decimals required).
pub fn amount(tok: &str) -> Option<i64> {
    let mut t = tok.trim();
    let mut neg = false;
    if let Some(x) = t.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        t = x;
        neg = true;
    }
    if let Some(x) = t.strip_suffix('-') {
        t = x;
        neg = true;
    }
    if let Some(x) = t.strip_prefix('-') {
        t = x;
        neg = true;
    }
    t = t.strip_prefix('$').unwrap_or(t);
    if let Some(x) = t.strip_prefix('-') {
        t = x;
        neg = true;
    }
    let (int, frac) = t.split_once(['.', ','])?;
    if int.is_empty() || int.len() > 5 || frac.len() != 2 {
        return None;
    }
    if !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let cents = int.parse::<i64>().ok()? * 100 + frac.parse::<i64>().ok()?;
    Some(if neg { -cents } else { cents })
}

/// A tax flag printed after an amount (`F`, `T`, `FA`, `*`).
fn is_flag(tok: &str) -> bool {
    let t = tok.trim();
    let letters = t.len() <= 2 && t.chars().all(|c| c.is_ascii_uppercase());
    let stars = t.chars().all(|c| c == '*');
    !t.is_empty() && (letters || stars)
}

/// The kind of an item code: `plu`, `upc` or `sku` (a chain's own code).
pub fn code_kind(tok: &str, p: &ChainProfile) -> Option<&'static str> {
    if !tok.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match tok.len() {
        4 => Some("plu"),
        5 if tok.starts_with('8') || tok.starts_with('9') => Some("plu"),
        n if p.sku_digits.contains(&n) => Some("sku"),
        8 | 11..=14 => Some("upc"),
        _ => None,
    }
}

const SIZE_UNITS: &[&str] = &[
    "oz", "lb", "lbs", "kg", "g", "gr", "ml", "l", "lt", "ltr", "floz", "gal", "qt", "pt", "ct", "pk", "pack", "dz",
    "count",
];
const WEIGHT_UNITS: &[&str] = &["lb", "lbs", "kg", "oz", "g"];

fn canonical_weight(u: &str) -> String {
    match u {
        "lbs" => "lb".into(),
        other => other.into(),
    }
}

/// Split glued number-unit tokens (`16OZ` -> `16 OZ`, `2.12lb` -> `2.12
/// lb`) and space out `@`.
fn tokens(line: &str) -> Vec<String> {
    let spaced = line.replace('@', " @ ");
    let mut out = Vec::new();
    for t in spaced.split_whitespace() {
        let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
        let (num, rest) = t.split_at(split);
        let unit = rest.trim_end_matches('.').to_lowercase();
        if !num.is_empty() && num.parse::<f64>().is_ok() && SIZE_UNITS.contains(&unit.as_str()) {
            out.push(num.to_string());
            out.push(rest.to_string());
        } else {
            out.push(t.to_string());
        }
    }
    out
}

/// Package sizes in an item text (`16 OZ`, `32 FL OZ`, `12 CT`): the text
/// without them, and the last size as `16 oz`.
fn take_size(toks: &[String]) -> (Vec<String>, Option<String>) {
    let mut keep = Vec::new();
    let mut size = None;
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        let next = toks.get(i + 1).map(|s| s.to_lowercase().trim_end_matches('.').to_string());
        if t.parse::<f64>().is_ok() {
            if next.as_deref() == Some("fl") && toks.get(i + 2).is_some_and(|u| u.eq_ignore_ascii_case("oz")) {
                size = Some(format!("{t} fl oz"));
                i += 3;
                continue;
            }
            if let Some(u) = next.filter(|u| SIZE_UNITS.contains(&u.as_str())) {
                let u = match u.as_str() {
                    "lbs" => "lb",
                    "gr" => "g",
                    "lt" | "ltr" => "l",
                    "floz" => "fl oz",
                    "count" => "ct",
                    "pack" => "pk",
                    x => x,
                };
                size = Some(format!("{t} {u}"));
                i += 2;
                continue;
            }
        }
        keep.push(t.clone());
        i += 1;
    }
    (keep, size)
}

/// Expand receipt-speak word by word (the chain's table over the shared
/// one) and title-case it: `ORG BNLS CHKN BRST` -> `Organic Boneless
/// Chicken Breast`.
pub fn expand(text: &str, p: &ChainProfile) -> String {
    let shared = &data().abbreviations;
    text.split_whitespace()
        .filter_map(|w| {
            let key = w.to_lowercase();
            let key = key.trim_matches(|c: char| !c.is_alphanumeric());
            if key.is_empty() {
                return None;
            }
            Some(p.abbreviations.get(key).or_else(|| shared.get(key)).cloned().unwrap_or_else(|| key.to_string()))
        })
        .flat_map(|w| w.split_whitespace().map(title).collect::<Vec<_>>())
        .collect::<Vec<_>>()
        .join(" ")
}

fn title(w: &str) -> String {
    let mut c = w.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect())
}

// ---------------------------------------------------------------------------
// Dates and times
// ---------------------------------------------------------------------------

/// A numeric date (`10/14/2026`, `10/14/26`, `2026-10-14`, `14-10-2026` with
/// `dmy`) as `YYYY-MM-DD`.
pub fn date_token(tok: &str, order: &str) -> Option<String> {
    let parts: Vec<&str> = tok.split(['/', '-', '.']).collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let n = |s: &str| s.parse::<u32>().ok();
    let year = |s: &str| match s.len() {
        2 => n(s).map(|y| 2000 + y as i32),
        4 => n(s).map(|y| y as i32),
        _ => None,
    };
    let (y, m, d) = if parts[0].len() == 4 {
        (year(parts[0])?, n(parts[1])?, n(parts[2])?)
    } else if order == "dmy" {
        (year(parts[2])?, n(parts[1])?, n(parts[0])?)
    } else {
        (year(parts[2])?, n(parts[0])?, n(parts[1])?)
    };
    chrono::NaiveDate::from_ymd_opt(y, m, d).map(|d| d.format("%Y-%m-%d").to_string())
}

/// A time (`17:42`, `5:42 PM`, `5:42PM`, `17:42:05`) as `HH:MM`, given the
/// token after it (for a separate AM/PM).
pub fn time_token(tok: &str, next: Option<&str>) -> Option<String> {
    let lower = tok.to_ascii_lowercase();
    let (body, glued) = match lower.strip_suffix("pm").or_else(|| lower.strip_suffix("am")) {
        Some(b) => (b.to_string(), Some(lower.ends_with("pm"))),
        None => (lower.clone(), None),
    };
    let parts: Vec<&str> = body.split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.len() > 2 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let mut h: u32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let pm = glued.or_else(|| match next.map(str::to_ascii_lowercase).as_deref() {
        Some("pm" | "p.m.") => Some(true),
        Some("am" | "a.m.") => Some(false),
        _ => None,
    });
    match pm {
        Some(true) if h < 12 => h += 12,
        Some(false) if h == 12 => h = 0,
        _ => {}
    }
    (h < 24 && m < 60).then(|| format!("{h:02}:{m:02}"))
}

// ---------------------------------------------------------------------------
// Lines
// ---------------------------------------------------------------------------

/// A quantity or weight: `2 @ 1.99`, `1.12 lb @ 2.99 /lb`.
#[derive(Debug, Clone, PartialEq)]
struct Measure {
    qty: f64,
    /// `ea`, or a weight unit.
    unit: String,
    /// Cents per unit.
    unit_price: i64,
}

impl Measure {
    fn total(&self) -> i64 {
        (self.qty * self.unit_price as f64).round() as i64
    }
}

/// Find a measure in a token list: (first token, one past the last, measure).
fn find_measure(toks: &[String]) -> Option<(usize, usize, Measure)> {
    let low: Vec<String> = toks.iter().map(|t| t.to_lowercase()).collect();
    for i in 1..low.len() {
        let at = low[i] == "@" || (low[i] == "x" && low[i - 1].parse::<u32>().is_ok());
        if !at {
            continue;
        }
        // the price after: `2.99`, `$2.99/lb`, `2.99 /lb`, `2.99 / lb`, `2.99 lb`, `2.99 ea`
        let Some(after) = low.get(i + 1) else { continue };
        let (price_tok, glued_unit) = after.split_once('/').map_or((after.as_str(), None), |(a, u)| (a, Some(u)));
        let Some(price) = amount(price_tok).filter(|p| *p > 0) else { continue };
        let mut end = i + 2;
        let mut per: Option<String> = glued_unit.filter(|u| !u.is_empty()).map(str::to_string);
        if per.is_none() {
            match low.get(end).map(String::as_str) {
                Some("/") => {
                    per = low.get(end + 1).cloned();
                    end += 2;
                }
                Some(t) if t.starts_with('/') && t.len() > 1 => {
                    per = Some(t[1..].to_string());
                    end += 1;
                }
                Some(t) if WEIGHT_UNITS.contains(&t) || matches!(t, "ea" | "each") => {
                    per = Some(t.to_string());
                    end += 1;
                }
                _ => {}
            }
        }
        // the quantity before: `1.12 lb`, `2`
        let before = &low[i - 1];
        if WEIGHT_UNITS.contains(&before.as_str()) && i >= 2 {
            if let Ok(q) = low[i - 2].parse::<f64>() {
                let unit = canonical_weight(before);
                if per.as_deref().is_some_and(|p| canonical_weight(p) != unit && !matches!(p, "ea" | "each")) {
                    continue;
                }
                return Some((i - 2, end, Measure { qty: q, unit, unit_price: price }));
            }
        }
        if let Ok(q) = before.parse::<u32>() {
            if q == 0 {
                continue;
            }
            if per.as_deref().is_some_and(|p| WEIGHT_UNITS.contains(&p)) {
                // `2 @ 1.99/lb`: a count at a per-pound price is not a measure we trust
                continue;
            }
            let mut start = i - 1;
            if start > 0 && matches!(low[start - 1].as_str(), "qty" | "quantity") {
                start -= 1;
            }
            return Some((start, end, Measure { qty: f64::from(q), unit: "ea".into(), unit_price: price }));
        }
    }
    None
}

/// Strip a trailing amount (and tax flags after it) from a token list.
fn take_amount(toks: &mut Vec<String>) -> Option<i64> {
    let mut flags = 0;
    while flags < 2 && toks.len() > flags + 1 && is_flag(&toks[toks.len() - 1 - flags]) {
        flags += 1;
    }
    for f in (0..=flags).rev() {
        let idx = toks.len().checked_sub(1 + f)?;
        if let Some(a) = amount(&toks[idx]) {
            toks.truncate(idx);
            return Some(a);
        }
    }
    None
}

/// One classified receipt line.
#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Blank,
    /// Text with no amount.
    Text(String),
    Item {
        toks: Vec<String>,
        amount: Option<i64>,
        measure: Option<Measure>,
    },
    Measure {
        m: Measure,
        total: Option<i64>,
    },
    Discount(i64),
    FinalPrice(i64),
    Void {
        toks: Vec<String>,
        amount: Option<i64>,
    },
    Fee {
        text: String,
        amount: i64,
    },
    Subtotal(i64),
    Tax(i64),
    Total(i64),
    Info,
}

const SUBTOTAL: &[&str] = &["subtotal", "sub total", "sub ttl", "subttl"];
const TAX: &[&str] = &["tax", "sales tax", "hst", "gst", "vat"];
const TOTAL: &[&str] = &["total", "balance due", "amount due", "total due", "balance", "ttl"];
const NOT_TOTAL: &[&str] =
    &["total savings", "total saved", "total items", "total number", "total discount", "total coupons", "items"];
const INFO: &[&str] = &[
    "visa",
    "mastercard",
    "amex",
    "discover",
    "debit",
    "credit",
    "cash",
    "change",
    "change due",
    "tend",
    "tendered",
    "card",
    "items sold",
    "item count",
    "you saved",
    "saved",
    "approval",
    "auth",
    "payment",
    "ebt",
    "snap",
];
const VOID: &[&str] = &["void", "voided", "item void", "cancel", "cancelled", "error correct"];

fn classify(raw: &str, p: &ChainProfile, after_totals: bool) -> Kind {
    let mut toks = tokens(raw);
    if toks.is_empty() {
        return Kind::Blank;
    }
    let ws = words(raw);
    let d = data();
    if let Some((start, end, m)) = find_measure(&toks) {
        let mut rest: Vec<String> = toks[end..].to_vec();
        let total = take_amount(&mut rest);
        let prefix: Vec<String> = toks[..start]
            .iter()
            .filter(|t| !matches!(t.to_lowercase().as_str(), "net" | "wt" | "weight" | "qty"))
            .cloned()
            .collect();
        if !after_totals && prefix.iter().any(|t| t.chars().any(char::is_alphabetic)) {
            return Kind::Item { toks: prefix, amount: total, measure: Some(m) };
        }
        if !after_totals {
            return Kind::Measure { m, total };
        }
    }
    let amt = take_amount(&mut toks);
    let Some(a) = amt else {
        return if after_totals { Kind::Info } else { Kind::Text(toks.join(" ")) };
    };
    if any_phrase(&ws, SUBTOTAL) {
        return Kind::Subtotal(a);
    }
    if any_phrase(&ws, TAX) && !ws.iter().any(|w| w == "taxable") {
        return Kind::Tax(a);
    }
    if any_phrase(&ws, TOTAL) && !any_phrase(&ws, NOT_TOTAL) {
        return Kind::Total(a);
    }
    if after_totals || any_phrase(&ws, INFO) {
        return Kind::Info;
    }
    if any_phrase(&ws, &p.final_price_words) {
        return Kind::FinalPrice(a.abs());
    }
    if any_phrase(&ws, VOID) {
        let toks = toks.into_iter().filter(|t| !any_phrase(&words(t), VOID)).collect();
        return Kind::Void { toks, amount: Some(a.abs()) };
    }
    if a < 0 || any_phrase(&ws, &d.discount_words) || any_phrase(&ws, &p.discount_words) {
        return Kind::Discount(a.abs());
    }
    let multi = |list: &[String]| list.iter().any(|f| words(f).len() > 1 && has_phrase(&ws, f));
    let single = |list: &[String]| list.iter().any(|f| words(f).len() == 1 && has_phrase(&ws, f));
    if multi(&d.fee_words) || multi(&p.fee_words) || ((single(&d.fee_words) || single(&p.fee_words)) && a < 100) {
        return Kind::Fee { text: toks.join(" "), amount: a };
    }
    Kind::Item { toks, amount: Some(a), measure: None }
}

/// One purchase line of a receipt.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Line {
    /// 1-based line number in the receipt text.
    pub n: usize,
    /// The receipt's text (code, amount and size taken out).
    pub text: String,
    /// The expansion (`Organic Bananas`).
    pub name: String,
    pub code: Option<String>,
    /// `plu`, `upc` or `sku`.
    pub code_kind: Option<String>,
    /// Count, or weight.
    pub qty: f64,
    /// `ea`, or the weight unit.
    pub unit: String,
    /// Package size from the text (`16 oz`).
    pub size: Option<String>,
    /// Cents per unit, as printed (before discounts).
    pub unit_price: Option<i64>,
    /// What was paid for the line, in cents, after discounts.
    pub total: i64,
    /// Discounts taken off it, in cents.
    pub discount: i64,
    pub voided: bool,
    /// The printed amount (before discounts).
    #[serde(skip)]
    pub amount: Option<i64>,
    #[serde(skip)]
    final_price: Option<i64>,
    #[serde(skip)]
    measured: bool,
}

/// A line that is not a purchase (fee, unmatched void).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    pub n: usize,
    pub text: String,
    /// `fee`, `void`.
    pub why: String,
    #[serde(with = "dollars_opt")]
    pub amount: Option<i64>,
}

/// A parsed receipt.
#[derive(Debug, Clone, Default)]
pub struct Receipt {
    /// The chain profile used (`generic` when none was detected).
    pub profile: String,
    /// The chain detected from the header (None: generic).
    pub chain: Option<String>,
    pub header: Vec<String>,
    pub date: Option<String>,
    pub time: Option<String>,
    pub subtotal: Option<i64>,
    pub tax: Option<i64>,
    pub total: Option<i64>,
    pub lines: Vec<Line>,
    pub skipped: Vec<Skipped>,
    pub warnings: Vec<String>,
}

impl Receipt {
    /// Lines that can become lots (not voided).
    pub fn kept(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter().filter(|l| !l.voided)
    }
}

fn apply_measure(l: &mut Line, m: &Measure, total: Option<i64>) {
    // an item line that printed the unit price takes the measure's total
    let unit_amount = m.qty != 1.0 && l.amount == Some(m.unit_price);
    l.qty = m.qty;
    l.unit.clone_from(&m.unit);
    l.unit_price = Some(m.unit_price);
    l.measured = true;
    if l.amount.is_none() || unit_amount {
        l.amount = Some(total.unwrap_or_else(|| m.total()));
    }
}

/// Does a quantity or weight line belong to the item above it? When that
/// item has no amount, or its amount is the line's total (else quantity x
/// price) or the unit price.
fn fits(l: &Line, m: &Measure, total: Option<i64>) -> bool {
    let near = |a: i64, b: i64| (a - b).abs() <= 1;
    !l.measured
        && !l.voided
        && l.amount.is_none_or(|a| near(a, total.unwrap_or_else(|| m.total())) || a == m.unit_price)
}

fn new_line(n: usize, toks: &[String], amount: Option<i64>, p: &ChainProfile) -> Line {
    let mut toks: Vec<String> = toks.to_vec();
    let mut code = None;
    if let Some(k) = toks.first().and_then(|t| code_kind(t, p)) {
        code = Some((toks.remove(0), k));
    } else if toks.len() > 1 {
        if let Some(k) = toks.last().and_then(|t| code_kind(t, p)) {
            code = toks.pop().map(|t| (t, k));
        }
    }
    let (rest, size) = take_size(&toks);
    let text = toks.join(" ");
    Line {
        n,
        name: expand(&rest.join(" "), p),
        text,
        code_kind: code.as_ref().map(|c| c.1.to_string()),
        code: code.map(|c| c.0),
        qty: 1.0,
        unit: "ea".into(),
        size,
        amount,
        ..Default::default()
    }
}

/// Parse a receipt's text. `chain` forces a profile (else detected).
pub fn parse(text: &str, chain: Option<&'static ChainProfile>) -> Receipt {
    let lines: Vec<&str> = text.lines().collect();
    let head: Vec<&str> = lines.iter().copied().filter(|l| !l.trim().is_empty()).take(10).collect();
    let detected = detect_chain(&head);
    let p = chain.or(detected).unwrap_or_else(generic);
    let mut r =
        Receipt { profile: p.name.clone(), chain: (!p.is_generic()).then(|| p.name.clone()), ..Default::default() };
    // dates and times anywhere
    for l in &lines {
        let toks: Vec<&str> = l.split_whitespace().collect();
        for (i, t) in toks.iter().enumerate() {
            if r.date.is_none() {
                r.date = date_token(t, &p.date_order);
            }
            if r.time.is_none() {
                r.time = time_token(t, toks.get(i + 1).copied());
            }
        }
    }
    let kinds: Vec<Kind> = {
        let mut after = false;
        lines
            .iter()
            .map(|l| {
                let k = classify(l, p, after);
                after |= matches!(k, Kind::Subtotal(_) | Kind::Total(_) | Kind::Tax(_));
                k
            })
            .collect()
    };
    let mut pending: Option<(Measure, Option<i64>)> = None;
    let mut seen_item = false;
    for (i, k) in kinds.iter().enumerate() {
        let n = i + 1;
        match k {
            Kind::Blank => {}
            Kind::Text(t) => {
                let next = kinds[i + 1..].iter().find(|k| **k != Kind::Blank);
                if let Some(Kind::Measure { total: Some(_), .. }) = next {
                    let toks: Vec<String> = t.split_whitespace().map(str::to_string).collect();
                    r.lines.push(new_line(n, &toks, None, p));
                    seen_item = true;
                } else if !seen_item {
                    r.header.push(lines[i].trim().to_string());
                }
            }
            Kind::Item { toks, amount, measure } => {
                seen_item = true;
                let mut l = new_line(n, toks, *amount, p);
                if let Some(m) = measure {
                    apply_measure(&mut l, m, *amount);
                } else if let Some((m, total)) = pending.take() {
                    apply_measure(&mut l, &m, total);
                }
                r.lines.push(l);
            }
            Kind::Measure { m, total } => match r.lines.last_mut() {
                Some(l) if fits(l, m, *total) => apply_measure(l, m, *total),
                _ => pending = Some((m.clone(), *total)),
            },
            Kind::Discount(a) => match r.lines.iter_mut().rev().find(|l| !l.voided) {
                Some(l) => l.discount += a,
                None => r.warnings.push(format!("line {n}: a discount of {} before any item", fmt(*a))),
            },
            Kind::FinalPrice(a) => {
                if let Some(l) = r.lines.iter_mut().rev().find(|l| !l.voided) {
                    l.final_price = Some(*a);
                }
            }
            Kind::Void { toks, amount } => {
                let key = new_line(n, toks, *amount, p);
                let same = |l: &Line| {
                    !l.voided
                        && (key.code.is_some() && l.code == key.code
                            || !key.text.is_empty() && words(&l.text) == words(&key.text))
                        && (amount.is_none() || l.amount == *amount)
                };
                let target = if key.code.is_none() && key.text.is_empty() {
                    r.lines.iter().rposition(|l| !l.voided && (amount.is_none() || l.amount == *amount))
                } else {
                    r.lines.iter().rposition(same)
                };
                match target {
                    Some(t) => r.lines[t].voided = true,
                    None => {
                        r.warnings.push(format!("line {n}: a void with no item to cancel"));
                        r.skipped.push(Skipped {
                            n,
                            text: lines[i].trim().to_string(),
                            why: "void".into(),
                            amount: *amount,
                        });
                    }
                }
            }
            Kind::Fee { text, amount } => {
                r.skipped.push(Skipped { n, text: text.clone(), why: "fee".into(), amount: Some(*amount) })
            }
            Kind::Subtotal(a) => {
                r.subtotal.get_or_insert(*a);
            }
            Kind::Tax(a) => *r.tax.get_or_insert(0) += a,
            Kind::Total(a) => {
                r.total.get_or_insert(*a);
            }
            Kind::Info => {
                if !seen_item {
                    r.header.push(lines[i].trim().to_string());
                }
            }
        }
    }
    for l in &mut r.lines {
        let amount = l.amount.unwrap_or(0);
        l.total = l.final_price.unwrap_or(amount - l.discount);
        if l.final_price.is_some() {
            l.discount = (amount - l.total).max(0);
        }
        if l.unit_price.is_none() && l.qty > 0.0 && l.amount.is_some() {
            l.unit_price = Some((amount as f64 / l.qty).round() as i64);
        }
    }
    // the kept lines (and fees) should add up to the subtotal
    let kept: i64 = r.kept().map(|l| l.total).sum::<i64>()
        + r.skipped.iter().filter(|s| s.why == "fee").filter_map(|s| s.amount).sum::<i64>();
    let want = r.subtotal.or_else(|| r.total.map(|t| t - r.tax.unwrap_or(0)));
    if let Some(w) = want {
        if (w - kept).abs() > 1 {
            r.warnings.push(format!(
                "the lines add up to {}, the receipt says {} (an OCR misread? check the review)",
                fmt(kept),
                fmt(w)
            ));
        }
    }
    if r.date.is_none() {
        r.warnings.push("no purchase date found on the receipt".into());
    }
    r
}

fn fmt(cents: i64) -> String {
    format!("${}", crate::util::fmt_money(cents))
}

/// The receipt fingerprint: SHA-256 over the store (else chain), date,
/// time, total and every item line's code, text and total.
pub fn fingerprint(store: Option<&str>, r: &Receipt, date: Option<&str>) -> String {
    let mut h = Sha256::new();
    let mut put = |s: &str| {
        h.update(s.as_bytes());
        h.update([0x1f]);
    };
    put(&store.map(str::to_lowercase).unwrap_or_default());
    put(date.unwrap_or_default());
    put(r.time.as_deref().unwrap_or_default());
    put(&r.total.map(|t| t.to_string()).unwrap_or_default());
    for l in r.kept() {
        put(l.code.as_deref().unwrap_or_default());
        put(&words(&l.text).join(" "));
        put(&l.total.to_string());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Serde helpers: cents as dollars in JSON.
pub mod dollars {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &i64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(crate::util::money_value(*v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
        Ok((f64::deserialize(d)? * 100.0).round() as i64)
    }
}

/// Serde helpers: optional cents as dollars in JSON.
pub mod dollars_opt {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(c) => s.serialize_f64(crate::util::money_value(*c)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
        Ok(Option::<f64>::deserialize(d)?.map(|v| (v * 100.0).round() as i64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const SPROUTS: &str = include_str!("../tests/fixtures/receipts/sprouts.txt");

    pub const ALDI: &str = include_str!("../tests/fixtures/receipts/aldi.txt");

    #[test]
    fn data_loads() {
        let d = data();
        assert!(d.abbreviations.len() > 50);
        assert!(profile("sprouts").is_some() && profile("ALDI").is_some());
        assert!(generic().detect.is_empty());
        for p in &d.chain {
            assert!(matches!(p.date_order.as_str(), "mdy" | "dmy"), "{}", p.name);
        }
        let mut keys: Vec<&String> = d.abbreviations.keys().collect();
        keys.dedup();
        assert!(keys.iter().all(|k| k.chars().all(|c| c.is_ascii_alphanumeric()) && **k == k.to_lowercase()));
    }

    #[test]
    fn amounts_and_codes() {
        assert_eq!(amount("2.99"), Some(299));
        assert_eq!(amount("$2.99"), Some(299));
        assert_eq!(amount("-1.00"), Some(-100));
        assert_eq!(amount("1.00-"), Some(-100));
        assert_eq!(amount("(0.50)"), Some(-50));
        assert_eq!(amount("-$0.50"), Some(-50));
        assert_eq!(amount("2,99"), Some(299));
        assert_eq!(amount("299"), None);
        assert_eq!(amount("2.9"), None);
        let g = generic();
        assert_eq!(code_kind("4011", g), Some("plu"));
        assert_eq!(code_kind("94011", g), Some("plu"));
        assert_eq!(code_kind("012345678905", g), Some("upc"));
        assert_eq!(code_kind("701234", profile("aldi").unwrap()), Some("sku"));
        assert_eq!(code_kind("12", g), None);
        assert_eq!(code_kind("12B", g), None);
    }

    #[test]
    fn abbreviations_expand() {
        let g = generic();
        assert_eq!(expand("ORG BNLS SKNLS CHKN BRST", g), "Organic Boneless Skinless Chicken Breast");
        assert_eq!(expand("GRK YGRT PLAIN", g), "Greek Yogurt Plain");
        assert_eq!(expand("EVOO", g), "Extra Virgin Olive Oil");
        let aldi = profile("aldi").unwrap();
        assert_eq!(expand("HF WHL MILK", aldi), "Happy Farms Whole Milk");
        assert_eq!(expand("HF WHL MILK", g), "Hf Whole Milk", "chain abbreviations stay with the chain");
    }

    #[test]
    fn dates_and_times() {
        assert_eq!(date_token("10/14/2026", "mdy").as_deref(), Some("2026-10-14"));
        assert_eq!(date_token("10/14/26", "mdy").as_deref(), Some("2026-10-14"));
        assert_eq!(date_token("2026-10-14", "mdy").as_deref(), Some("2026-10-14"));
        assert_eq!(date_token("14-10-2026", "dmy").as_deref(), Some("2026-10-14"));
        assert_eq!(date_token("14/10/2026", "mdy"), None);
        assert_eq!(time_token("5:42", Some("PM")).as_deref(), Some("17:42"));
        assert_eq!(time_token("12:05AM", None).as_deref(), Some("00:05"));
        assert_eq!(time_token("17:42:05", None).as_deref(), Some("17:42"));
        assert_eq!(time_token("010-0100", None), None);
    }

    #[test]
    fn chains_are_detected_from_the_header() {
        assert_eq!(detect_chain(&["SPROUTS FARMERS MARKET", "Store #12"]).map(|p| p.name.as_str()), Some("Sprouts"));
        assert_eq!(detect_chain(&["  ALDI  ", "1234 Example Rd"]).map(|p| p.name.as_str()), Some("ALDI"));
        assert!(detect_chain(&["Valdivia Bakery"]).is_none(), "whole words only");
        let r = parse("CORNER GROCER\nMILK 3.00\nTOTAL 3.00\n", None);
        assert_eq!((r.profile.as_str(), r.chain.as_deref()), ("generic", None));
    }

    #[test]
    fn sprouts_style_receipt() {
        let r = parse(SPROUTS, None);
        assert_eq!(r.chain.as_deref(), Some("Sprouts"));
        assert_eq!(r.date.as_deref(), Some("2026-10-14"));
        assert_eq!(r.time.as_deref(), Some("17:42"));
        assert_eq!((r.subtotal, r.tax, r.total), (Some(2281), Some(0), Some(2281)));
        assert!(r.header.iter().any(|h| h.contains("100 Example Blvd")), "{:?}", r.header);
        let l: Vec<&Line> = r.lines.iter().collect();
        assert_eq!(l.len(), 6, "{l:#?}");
        // a weight line under a priceless item
        assert_eq!((l[0].name.as_str(), l[0].qty, l[0].unit.as_str()), ("Organic Bananas", 2.12, "lb"));
        assert_eq!((l[0].unit_price, l[0].total), (Some(99), 210));
        // a PLU and a multi-quantity line
        assert_eq!((l[1].code.as_deref(), l[1].code_kind.as_deref()), (Some("4046"), Some("plu")));
        assert_eq!((l[1].qty, l[1].unit_price, l[1].total), (3.0, Some(125), 375));
        assert_eq!(l[2].code.as_deref(), Some("94011"));
        // a coupon
        assert_eq!((l[3].name.as_str(), l[3].size.as_deref()), ("Greek Yogurt Plain", Some("32 oz")));
        assert_eq!((l[3].total, l[3].discount), (449, 100));
        // a void
        assert!(l[4].voided && l[4].name == "Walnut Halves");
        assert_eq!((l[5].name.as_str(), l[5].qty, l[5].total), ("Boneless Skinless Chicken Breast", 1.5, 1049));
        assert_eq!(r.skipped, vec![Skipped { n: 18, text: "PAPER BAG".into(), why: "fee".into(), amount: Some(10) }]);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn aldi_style_receipt() {
        let r = parse(ALDI, None);
        assert_eq!(r.chain.as_deref(), Some("ALDI"));
        assert_eq!((r.date.as_deref(), r.time.as_deref()), (Some("2026-10-20"), Some("09:15")));
        let l: Vec<&Line> = r.lines.iter().collect();
        assert_eq!(l.len(), 5, "{l:#?}");
        assert_eq!((l[0].code.as_deref(), l[0].code_kind.as_deref()), (Some("701234"), Some("sku")));
        assert_eq!(l[0].name, "Happy Farms Whole Milk Gal");
        assert_eq!((l[0].qty, l[0].total), (1.0, 315));
        // `2 @ 1.29` printed before its item
        assert_eq!(
            (l[1].name.as_str(), l[1].qty, l[1].unit_price, l[1].total),
            ("Organic Baby Spinach", 2.0, Some(129), 258)
        );
        // PRICE YOU PAY after instant savings
        assert_eq!((l[2].name.as_str(), l[2].size.as_deref()), ("Large Brown Eggs", Some("12 ct")));
        assert_eq!((l[2].total, l[2].discount), (369, 50));
        assert!(l[3].voided, "voided by code");
        assert_eq!((l[4].name.as_str(), l[4].size.as_deref(), l[4].total), ("Bananas Bag", Some("2 lb"), 139));
        assert_eq!(r.skipped.len(), 1);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn mismatched_totals_warn() {
        let r = parse("SHOP\n01/02/2026\nMILK 3.00\nEGGS 4.00\nSUBTOTAL 9.00\n", None);
        assert!(r.warnings.iter().any(|w| w.contains("add up to $7.00")), "{:?}", r.warnings);
    }

    #[test]
    fn fingerprints_are_stable_and_sensitive() {
        let r = parse(SPROUTS, None);
        let a = fingerprint(Some("sprouts-north"), &r, r.date.as_deref());
        assert_eq!(a, fingerprint(Some("SPROUTS-NORTH"), &r, r.date.as_deref()));
        assert_eq!(a.len(), 64);
        assert_ne!(a, fingerprint(Some("sprouts-south"), &r, r.date.as_deref()));
        assert_ne!(a, fingerprint(Some("sprouts-north"), &r, Some("2026-10-15")));
    }
}
