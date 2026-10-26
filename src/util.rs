//! Small pure helpers: dates, durations, tags, slugs, money and quantities.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Offset, Utc};

use crate::error::{AppError, Result};

/// A configured time zone: the system local zone, an IANA zone, or a fixed offset.
#[derive(Debug, Clone)]
pub enum Tz {
    Local,
    Named(chrono_tz::Tz),
    Fixed(chrono::FixedOffset),
}

impl Tz {
    pub fn parse(s: &str) -> Result<Self> {
        let t = s.trim();
        match t.to_ascii_lowercase().as_str() {
            "" | "local" => Ok(Self::Local),
            "utc" | "z" => Ok(Self::Fixed(Utc.fix())),
            _ => t
                .parse::<chrono_tz::Tz>()
                .map(Self::Named)
                .or_else(|_| {
                    DateTime::parse_from_str(&format!("2000-01-01T00:00:00{t}"), "%Y-%m-%dT%H:%M:%S%:z")
                        .map(|d| Self::Fixed(*d.offset()))
                })
                .map_err(|_| {
                    AppError::config(format!("unknown timezone '{s}' (use local, UTC, an IANA name, or +HH:MM)"))
                }),
        }
    }

    /// Current wall-clock time in this zone.
    pub fn now(&self) -> NaiveDateTime {
        self.from_utc(&utc_now())
    }

    pub fn from_utc(&self, t: &DateTime<Utc>) -> NaiveDateTime {
        match self {
            Self::Local => t.with_timezone(&chrono::Local).naive_local(),
            Self::Named(z) => t.with_timezone(z).naive_local(),
            Self::Fixed(o) => t.with_timezone(o).naive_local(),
        }
    }

    pub fn today(&self) -> NaiveDate {
        self.now().date()
    }
}

/// Current UTC time, or `$SOURCE_DATE_EPOCH` (Unix seconds) when set, so that
/// output such as `generated_at` can be made reproducible.
pub fn utc_now() -> DateTime<Utc> {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .and_then(|secs| DateTime::from_timestamp(secs, 0))
        .unwrap_or_else(Utc::now)
}

pub fn now_iso() -> String {
    utc_now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

const DATE_FMT: &str = "%Y-%m-%d";
const DATETIME_FMT: &str = "%Y-%m-%dT%H:%M:%S";

/// Parse a user-supplied date/time into the canonical stored form:
/// `YYYY-MM-DD` for dates, `YYYY-MM-DDTHH:MM:SS` for datetimes (wall-clock time
/// in the configured zone; RFC 3339 inputs with offsets are converted).
/// `extra` formats (e.g. `%m/%d/%Y`) are tried first.
pub fn parse_when(s: &str, extra: &[&str], tz: &Tz) -> Result<String> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("today") {
        return Ok(tz.today().format(DATE_FMT).to_string());
    }
    if t.eq_ignore_ascii_case("now") {
        return Ok(tz.now().format(DATETIME_FMT).to_string());
    }
    let as_date = |f: &str| NaiveDate::parse_from_str(t, f).ok().map(|d| d.format(DATE_FMT).to_string());
    let as_dt = |f: &str| NaiveDateTime::parse_from_str(t, f).ok().map(|d| d.format(DATETIME_FMT).to_string());
    let extra_hit = extra.iter().filter(|f| !f.is_empty()).find_map(|f| as_dt(f).or_else(|| as_date(f)));
    extra_hit
        .or_else(|| {
            DateTime::parse_from_rfc3339(t)
                .ok()
                .map(|d| tz.from_utc(&d.with_timezone(&Utc)).format(DATETIME_FMT).to_string())
        })
        .or_else(|| {
            ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"]
                .iter()
                .find_map(|f| as_dt(f))
        })
        .or_else(|| ["%Y-%m-%d", "%Y/%m/%d", "%Y%m%d"].iter().find_map(|f| as_date(f)))
        .map(|s| s.strip_suffix("T00:00:00").map_or(s.clone(), str::to_string))
        .ok_or_else(|| AppError::invalid(format!("cannot parse date '{s}' (expected YYYY-MM-DD or ISO 8601)")))
}

/// Parse a date-only value (for filters).
pub fn parse_date(s: &str, tz: &Tz) -> Result<NaiveDate> {
    parse_when(s, &[], tz).and_then(|w| date_of(&w))
}

/// Parse a user date into the stored `YYYY-MM-DD` form (any time part is dropped).
/// `extra` formats are tried first.
pub fn parse_day(s: &str, extra: &[&str], tz: &Tz) -> Result<String> {
    parse_when(s, extra, tz).and_then(|w| date_of(&w)).map(|d| d.format(DATE_FMT).to_string())
}

/// Parse an amount of money (`1299.99`, `$1,299.99`, `12 USD`) into whole cents.
pub fn parse_money(s: &str) -> Result<i64> {
    let bad = || AppError::invalid(format!("cannot parse amount '{s}' (e.g. 1299.99 or $1,299.99)"));
    let t: String =
        s.trim().trim_start_matches('$').trim_end_matches("USD").trim().chars().filter(|c| *c != ' ').collect();
    let t = normalize_number(t.trim_start_matches('$'));
    let (whole, frac) = t.split_once('.').unwrap_or((&t, ""));
    if whole.is_empty() && frac.is_empty()
        || !whole.chars().all(|c| c.is_ascii_digit())
        || !frac.chars().all(|c| c.is_ascii_digit())
    {
        return Err(bad());
    }
    let whole: i64 = if whole.is_empty() { 0 } else { whole.parse().map_err(|_| bad())? };
    let mut digits = frac.bytes().map(|b| i64::from(b - b'0'));
    let cents = digits.next().unwrap_or(0) * 10 + digits.next().unwrap_or(0);
    let round_up = digits.next().is_some_and(|d| d >= 5);
    whole.checked_mul(100).and_then(|w| w.checked_add(cents + i64::from(round_up))).ok_or_else(bad)
}

/// Cents as a plain decimal amount: `129999` -> `1299.99`.
pub fn fmt_money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", cents.abs() / 100, cents.abs() % 100)
}

/// Cents as dollars for JSON output.
pub fn money_value(cents: i64) -> f64 {
    cents as f64 / 100.0
}

/// Parse a (possibly fractional) quantity such as `2`, `0.5` or `1,5`.
pub fn parse_qty(s: &str) -> Result<f64> {
    normalize_number(s.trim())
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| AppError::invalid(format!("cannot parse quantity '{s}'")))
}

/// Shortest text for a quantity: `2` not `2.0`, no binary noise.
pub fn fmt_qty(q: f64) -> String {
    crate::sheet::fmt_number(q)
}

/// Days from `today` until the stored date `day` (negative when past).
pub fn days_until(day: &str, today: NaiveDate) -> Option<i64> {
    date_of(day).ok().map(|d| (d - today).num_days())
}

/// Date part of a stored `taken_at`.
pub fn date_of(taken_at: &str) -> Result<NaiveDate> {
    taken_at
        .get(..10)
        .and_then(|d| NaiveDate::parse_from_str(d, DATE_FMT).ok())
        .ok_or_else(|| AppError::invalid(format!("bad stored date '{taken_at}'")))
}

/// Render a stored `taken_at` with a user date format (time appended when present).
pub fn display_when(taken_at: &str, date_format: &str) -> String {
    match NaiveDateTime::parse_from_str(taken_at, DATETIME_FMT) {
        Ok(dt) => format!("{} {}", dt.date().format(date_format), dt.format("%H:%M")),
        Err(_) => date_of(taken_at).map_or_else(|_| taken_at.to_string(), |d| d.format(date_format).to_string()),
    }
}

/// Parse a window/duration like `30d`, `12w`, `6m`, `1y` into days.
pub fn parse_duration_days(s: &str) -> Result<f64> {
    let t = s.trim().to_ascii_lowercase();
    let (num, unit) = t.split_at(t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len()));
    let n: f64 =
        num.parse().map_err(|_| AppError::usage(format!("bad duration '{s}' (examples: 30d, 12w, 6m, 1y)")))?;
    let mult = match unit {
        "" | "d" | "day" | "days" => 1.0,
        "w" | "wk" | "week" | "weeks" => 7.0,
        "m" | "mo" | "month" | "months" => 30.436_875,
        "y" | "yr" | "year" | "years" => 365.2425,
        _ => return Err(AppError::usage(format!("bad duration unit in '{s}' (use d, w, m, y)"))),
    };
    Ok(n * mult)
}

/// A look-back span: whole calendar months (for `m`/`y`) or days (`d`/`w`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Span {
    Days(f64),
    Months(u32),
}

/// Parse `30d`, `12w`, `6m`, `1y`. Integral months/years are calendar spans,
/// so `1y` before 2024-01-01 is exactly 2023-01-01.
pub fn parse_span(s: &str) -> Result<Span> {
    let days = parse_duration_days(s)?;
    let t = s.trim().to_ascii_lowercase();
    let num: f64 = t.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().unwrap_or(0.0);
    let unit = t.trim_start_matches(|c: char| !c.is_ascii_alphabetic());
    let months = match unit.chars().next() {
        Some('m') => num,
        Some('y') => num * 12.0,
        _ => return Ok(Span::Days(days)),
    };
    Ok(if months.fract() == 0.0 && months >= 0.0 { Span::Months(months as u32) } else { Span::Days(days) })
}

/// The date `span` before `date`.
pub fn date_before(date: NaiveDate, span: Span) -> NaiveDate {
    match span {
        Span::Days(d) => date - chrono::Duration::days(d.round() as i64),
        Span::Months(m) => date.checked_sub_months(chrono::Months::new(m)).unwrap_or(NaiveDate::MIN),
    }
}

/// The date `span` after `date`.
pub fn date_after(date: NaiveDate, span: Span) -> NaiveDate {
    match span {
        Span::Days(d) => date + chrono::Duration::days(d.round() as i64),
        Span::Months(m) => date.checked_add_months(chrono::Months::new(m)).unwrap_or(NaiveDate::MAX),
    }
}

/// Parse a tag list from comma/semicolon separated text or a JSON array string.
pub fn parse_tags(s: &str) -> Vec<String> {
    let t = s.trim();
    serde_json::from_str::<Vec<String>>(t)
        .unwrap_or_else(|_| t.split([',', ';']).map(str::to_string).collect())
        .into_iter()
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

pub fn tags_to_json(tags: &[String]) -> String {
    serde_json::to_string(tags).unwrap_or_else(|_| "[]".into())
}

pub fn tags_from_db(s: Option<String>) -> Vec<String> {
    s.map(|s| parse_tags(&s)).unwrap_or_default()
}

/// Merge tags: add then remove, de-duplicated, order preserving.
pub fn merge_tags(existing: &[String], add: &[String], remove: &[String]) -> Vec<String> {
    existing.iter().chain(add.iter()).filter(|t| !remove.contains(t)).fold(Vec::new(), |mut acc, t| {
        if !acc.contains(t) {
            acc.push(t.clone());
        }
        acc
    })
}

/// Lowercase, dash-separated slug.
pub fn slugify(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

pub fn validate_slug(s: &str) -> Result<String> {
    let t = s.trim();
    if !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        Ok(t.to_lowercase())
    } else {
        Err(AppError::invalid(format!("invalid slug '{s}' (letters, digits, '-', '_', '.'; e.g. '{}')", slugify(s))))
    }
}

/// `1,000` -> `1000` (thousands separator), `2,1` / `0,125` -> decimal comma.
/// A single comma is a thousands separator only when it is followed by exactly
/// three digits and preceded by a non-zero group of 1-3 digits.
fn normalize_number(s: &str) -> String {
    let thousands_group = |s: &str| {
        let (head, tail) = s.split_once(',').unwrap_or((s, ""));
        let head = head.trim_start_matches(['-', '+']);
        tail.len() == 3 && (1..=3).contains(&head.len()) && head.chars().all(|c| c.is_ascii_digit()) && head != "0"
    };
    match (s.matches(',').count(), s.contains('.')) {
        (1, false) if !thousands_group(s) => s.replace(',', "."),
        (n, _) if n > 0 => s.replace(',', ""),
        _ => s.to_string(),
    }
}

pub fn parse_bool(s: &str) -> Result<Option<bool>> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "null" | "na" | "n/a" | "unknown" => Ok(None),
        "1" | "true" | "yes" | "y" | "t" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "n" | "f" | "off" => Ok(Some(false)),
        other => Err(AppError::invalid(format!("cannot parse boolean '{other}'"))),
    }
}

/// New import batch identifier: `b<UTC timestamp>-<nanos hex>`.
pub fn new_batch_id() -> String {
    let now = Utc::now();
    format!("b{}-{:05x}", now.format("%Y%m%dT%H%M%S"), now.timestamp_subsec_nanos() & 0xfffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dates() {
        let tz = Tz::parse("UTC").unwrap();
        assert_eq!(parse_when("2024-03-05", &[], &tz).unwrap(), "2024-03-05");
        assert_eq!(parse_when("2024/03/05", &[], &tz).unwrap(), "2024-03-05");
        assert_eq!(parse_when("2024-03-05 07:30", &[], &tz).unwrap(), "2024-03-05T07:30:00");
        assert_eq!(parse_when("2024-03-05T00:00:00", &[], &tz).unwrap(), "2024-03-05");
        assert_eq!(parse_when("03/05/2024", &["%m/%d/%Y"], &tz).unwrap(), "2024-03-05");
        let ny = Tz::parse("America/New_York").unwrap();
        assert_eq!(parse_when("2024-03-05T12:00:00Z", &[], &ny).unwrap(), "2024-03-05T07:00:00");
        assert!(parse_when("not a date", &[], &tz).is_err());
    }

    #[test]
    fn parses_money_and_quantities() {
        assert_eq!(parse_money("1299.99").unwrap(), 129_999);
        assert_eq!(parse_money("$1,299.99").unwrap(), 129_999);
        assert_eq!(parse_money("12 USD").unwrap(), 1200);
        assert_eq!(parse_money("0.5").unwrap(), 50);
        assert_eq!(parse_money("3.455").unwrap(), 346);
        assert_eq!(parse_money("2,5").unwrap(), 250);
        assert!(parse_money("abc").is_err());
        assert!(parse_money("-5").is_err());
        assert_eq!(fmt_money(129_999), "1299.99");
        assert_eq!(fmt_money(5), "0.05");
        assert_eq!(parse_qty("1,5").unwrap(), 1.5);
        assert_eq!(fmt_qty(2.0), "2");
        assert_eq!(fmt_qty(0.25), "0.25");
    }

    #[test]
    fn durations_and_tags() {
        assert_eq!(parse_duration_days("30d").unwrap(), 30.0);
        assert_eq!(parse_duration_days("2w").unwrap(), 14.0);
        assert!((parse_duration_days("1y").unwrap() - 365.2425).abs() < 1e-9);
        assert!(parse_duration_days("3x").is_err());
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(date_before(d("2024-01-01"), parse_span("1y").unwrap()), d("2023-01-01"));
        assert_eq!(date_before(d("2024-07-01"), parse_span("6m").unwrap()), d("2024-01-01"));
        assert_eq!(date_before(d("2024-01-15"), parse_span("2w").unwrap()), d("2024-01-01"));
        assert_eq!(parse_span("1.5m").unwrap(), Span::Days(1.5 * 30.436_875));
        assert_eq!(parse_tags("a, b;c"), vec!["a", "b", "c"]);
        assert_eq!(parse_tags(r#"["x","y"]"#), vec!["x", "y"]);
        assert_eq!(merge_tags(&["a".into()], &["b".into(), "a".into()], &["c".into()]), vec!["a", "b"]);
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("  Jane Doe! "), "jane-doe");
        assert!(validate_slug("jane-doe").is_ok());
        assert!(validate_slug("jane doe").is_err());
    }

    #[test]
    fn display_and_days() {
        assert_eq!(display_when("2024-03-05", "%d.%m.%Y"), "05.03.2024");
        assert_eq!(display_when("2024-03-05T07:30:00", "%Y-%m-%d"), "2024-03-05 07:30");
        let today = NaiveDate::from_ymd_opt(2024, 3, 1).unwrap();
        assert_eq!(days_until("2024-03-05", today), Some(4));
        assert_eq!(days_until("2024-02-28", today), Some(-2));
    }
}
