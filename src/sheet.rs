//! Reading spreadsheets (xlsx, xlsm, xlsb, xls, ods) into a plain cell grid.
//!
//! Cells are reduced to text, numbers and dates; everything else (formulas'
//! cached values aside) is irrelevant for inventory rows. Grid coordinates are
//! absolute: `rows[0][0]` is A1 even when the sheet's used range starts later,
//! so column letters and row numbers match what the user sees in Excel.

use std::path::Path;

use calamine::{open_workbook_auto, Data, Reader};
use chrono::{Duration, NaiveDate};

use crate::error::{AppError, Result};

pub const EXTENSIONS: &[&str] = &["xlsx", "xlsm", "xlsb", "xls", "ods"];

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Empty,
    Number(f64),
    Text(String),
    Date(NaiveDate),
}

impl Cell {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Text(t) => t.trim().is_empty(),
            _ => false,
        }
    }

    /// Text as an importer sees it: numbers without a spurious `.0`, dates as ISO.
    pub fn text(&self) -> String {
        match self {
            Self::Empty => String::new(),
            Self::Number(n) => fmt_number(*n),
            Self::Text(t) => t.trim().to_string(),
            Self::Date(d) => d.format("%Y-%m-%d").to_string(),
        }
    }
}

/// Shortest decimal for a cell number (`5` not `5.0`, and no binary noise from
/// spreadsheet arithmetic such as `0.30000000000000004`).
pub fn fmt_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{:.10}", n);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[derive(Debug, Clone)]
pub struct Grid {
    pub name: String,
    pub rows: Vec<Vec<Cell>>,
}

static EMPTY: Cell = Cell::Empty;

impl Grid {
    pub fn cell(&self, row: usize, col: usize) -> &Cell {
        self.rows.get(row).and_then(|r| r.get(col)).unwrap_or(&EMPTY)
    }

    pub fn width(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SheetInfo {
    pub index: usize,
    pub name: String,
    pub rows: usize,
    pub columns: usize,
    /// Used range, e.g. `A1:H40` (empty for an empty sheet).
    pub dimensions: String,
}

pub fn is_spreadsheet(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn open(path: &Path) -> Result<calamine::Sheets<std::io::BufReader<std::fs::File>>> {
    open_workbook_auto(path).map_err(|e| AppError::io(format!("reading spreadsheet {}: {e}", path.display())))
}

fn range_of(wb: &mut calamine::Sheets<std::io::BufReader<std::fs::File>>, name: &str) -> Result<calamine::Range<Data>> {
    wb.worksheet_range(name).map_err(|e| AppError::invalid(format!("sheet '{name}': {e}")))
}

pub fn list_sheets(path: &Path) -> Result<Vec<SheetInfo>> {
    let mut wb = open(path)?;
    wb.sheet_names()
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let r = range_of(&mut wb, &name)?;
            let dimensions = match (r.start(), r.end()) {
                (Some((r0, c0)), Some((r1, c1))) => {
                    format!("{}{}:{}{}", col_name(c0 as usize), r0 + 1, col_name(c1 as usize), r1 + 1)
                }
                _ => String::new(),
            };
            let (rows, columns) = r.get_size();
            Ok(SheetInfo { index: index + 1, name, rows, columns, dimensions })
        })
        .collect()
}

/// Read one sheet (by name, case-insensitive; default: the first sheet).
pub fn read(path: &Path, sheet: Option<&str>) -> Result<Grid> {
    let mut wb = open(path)?;
    let names = wb.sheet_names();
    let name = match sheet {
        None => names.first().cloned().ok_or_else(|| AppError::invalid("workbook has no sheets"))?,
        Some(want) => names
            .iter()
            .find(|n| n.eq_ignore_ascii_case(want.trim()))
            .cloned()
            .ok_or_else(|| AppError::not_found(format!("no sheet '{want}' (sheets: {})", names.join(", "))))?,
    };
    let range = range_of(&mut wb, &name)?;
    let (r0, c0) = range.start().map_or((0, 0), |(r, c)| (r as usize, c as usize));
    let mut rows: Vec<Vec<Cell>> = vec![Vec::new(); r0];
    rows.extend(
        range.rows().map(|row| std::iter::repeat_n(Cell::Empty, c0).chain(row.iter().map(convert)).collect::<Vec<_>>()),
    );
    Ok(Grid { name, rows })
}

fn convert(d: &Data) -> Cell {
    match d {
        Data::Empty | Data::Error(_) => Cell::Empty,
        Data::Int(i) => Cell::Number(*i as f64),
        Data::Float(f) => Cell::Number(*f),
        Data::Bool(b) => Cell::Text(b.to_string()),
        Data::String(s) => {
            if s.trim().is_empty() {
                Cell::Empty
            } else {
                Cell::Text(s.clone())
            }
        }
        Data::DateTime(dt) if dt.is_datetime() => {
            let (y, m, d, ..) = dt.to_ymd_hms_milli();
            NaiveDate::from_ymd_opt(i32::from(y), u32::from(m), u32::from(d))
                .map_or_else(|| Cell::Number(dt.as_f64()), Cell::Date)
        }
        Data::DateTime(dt) => Cell::Number(dt.as_f64()),
        Data::DateTimeIso(s) => s.get(..10).and_then(parse_text_date).map_or_else(|| Cell::Text(s.clone()), Cell::Date),
        Data::DurationIso(s) => Cell::Text(s.clone()),
    }
}

/// Excel (1900 date system) serial number to a date. Only plausible draw dates
/// (1950..2150) count, so ordinary numbers are not mistaken for dates.
pub fn serial_date(n: f64) -> Option<NaiveDate> {
    if !(18_264.0..=91_311.0).contains(&n) || n.fract() > 0.999 {
        return None;
    }
    NaiveDate::from_ymd_opt(1899, 12, 30).map(|base| base + Duration::days(n.trunc() as i64))
}

const TEXT_DATE_FORMATS: &[&str] =
    &["%Y-%m-%d", "%Y/%m/%d", "%m/%d/%Y", "%m/%d/%y", "%m-%d-%Y", "%b %d, %Y", "%B %d, %Y", "%d-%b-%Y", "%d %b %Y"];

/// ISO or US-style date text (`2024-12-04`, `12/4/2024`, `Dec 4, 2024`).
pub fn parse_text_date(s: &str) -> Option<NaiveDate> {
    let t = s.trim();
    TEXT_DATE_FORMATS.iter().find_map(|f| NaiveDate::parse_from_str(t, f).ok()).filter(|d| {
        use chrono::Datelike;
        (1900..2200).contains(&d.year())
    })
}

/// A header cell read as a draw date: a date cell, an Excel serial, or date text.
pub fn header_date(c: &Cell) -> Option<NaiveDate> {
    match c {
        Cell::Date(d) => Some(*d),
        Cell::Number(n) => serial_date(*n),
        Cell::Text(t) => parse_text_date(t).or_else(|| t.trim().parse::<f64>().ok().and_then(serial_date)),
        Cell::Empty => None,
    }
}

/// `0` -> `A`, `27` -> `AB`.
pub fn col_name(mut idx: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (idx % 26) as u8);
        if idx < 26 {
            break;
        }
        idx = idx / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// `A` -> 0, `ab` -> 27; `None` unless the text is 1-3 ASCII letters.
pub fn col_index(s: &str) -> Option<usize> {
    let t = s.trim();
    if t.is_empty() || t.len() > 3 || !t.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    t.to_ascii_uppercase().bytes().try_fold(0usize, |acc, b| Some(acc * 26 + usize::from(b - b'A') + 1)).map(|n| n - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_and_letters() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(27), "AB");
        assert_eq!(col_index("A"), Some(0));
        assert_eq!(col_index("ab"), Some(27));
        assert_eq!(col_index("Units"), None);
        for i in [0, 5, 26, 51, 52, 701, 702] {
            assert_eq!(col_index(&col_name(i)), Some(i));
        }
    }

    #[test]
    fn dates_from_headers() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(serial_date(45630.0), Some(d("2024-12-04")));
        assert_eq!(serial_date(5.1), None);
        assert_eq!(header_date(&Cell::Number(45000.0)), Some(d("2023-03-15")));
        assert_eq!(header_date(&Cell::Text("12/4/2024".into())), Some(d("2024-12-04")));
        assert_eq!(header_date(&Cell::Text("2024-12-04".into())), Some(d("2024-12-04")));
        assert_eq!(header_date(&Cell::Text("Dec 4, 2024".into())), Some(d("2024-12-04")));
        assert_eq!(header_date(&Cell::Text("Units".into())), None);
        assert_eq!(fmt_number(5.0), "5");
        assert_eq!(fmt_number(0.1 + 0.2), "0.3");
        assert_eq!(fmt_number(-1.25), "-1.25");
    }
}
