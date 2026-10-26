//! A small PDF writer for text reports (the insurance inventory), on
//! `pdf-writer`. Pages are US Letter landscape; text is set in the built-in
//! Courier and Courier-Bold fonts (no font embedding, no external tools), so
//! a column is a fixed number of characters wide. Characters outside
//! Windows-1252 print as `?`.

use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str, TextStr};

const PAGE_W: f32 = 792.0;
const PAGE_H: f32 = 612.0;
const MARGIN: f32 = 36.0;
/// Courier glyphs are 0.6 em wide.
const ADVANCE: f32 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Style {
    Normal,
    Bold,
}

struct Line {
    text: String,
    size: f32,
    style: Style,
    rule: bool,
}

/// A document built line by line.
pub struct Doc {
    title: String,
    lines: Vec<Line>,
}

impl Doc {
    pub fn new(title: &str) -> Self {
        Self { title: title.to_string(), lines: Vec::new() }
    }

    /// Characters that fit on a line at `size` points.
    pub fn width_chars(size: f32) -> usize {
        ((PAGE_W - 2.0 * MARGIN) / (size * ADVANCE)) as usize
    }

    /// Add text, wrapped to the page width.
    pub fn text(&mut self, text: &str, size: f32, style: Style) {
        let width = Self::width_chars(size).max(10);
        for l in wrap(text, width) {
            self.lines.push(Line { text: l, size, style, rule: false });
        }
    }

    pub fn gap(&mut self) {
        self.lines.push(Line { text: String::new(), size: 4.0, style: Style::Normal, rule: false });
    }

    /// A horizontal rule.
    pub fn rule(&mut self) {
        self.lines.push(Line { text: String::new(), size: 4.0, style: Style::Normal, rule: true });
    }

    /// The finished PDF.
    pub fn render(&self) -> Vec<u8> {
        // Split into pages.
        let mut pages: Vec<Vec<&Line>> = vec![Vec::new()];
        let mut y = PAGE_H - MARGIN;
        for l in &self.lines {
            let h = l.size * 1.25;
            if y - h < MARGIN + 14.0 && !pages.last().is_some_and(Vec::is_empty) {
                pages.push(Vec::new());
                y = PAGE_H - MARGIN;
            }
            y -= h;
            pages.last_mut().expect("a page").push(l);
        }
        let n = pages.len();
        let mut pdf = Pdf::new();
        let (catalog, tree, regular, bold, info) = (Ref::new(1), Ref::new(2), Ref::new(3), Ref::new(4), Ref::new(5));
        let page_ids: Vec<Ref> = (0..n).map(|i| Ref::new(6 + 2 * i as i32)).collect();
        pdf.catalog(catalog).pages(tree);
        pdf.pages(tree).kids(page_ids.iter().copied()).count(n as i32);
        pdf.type1_font(regular).base_font(Name(b"Courier")).encoding_predefined(Name(b"WinAnsiEncoding"));
        pdf.type1_font(bold).base_font(Name(b"Courier-Bold")).encoding_predefined(Name(b"WinAnsiEncoding"));
        pdf.document_info(info).title(TextStr(&self.title)).creator(TextStr("wherebox-cli"));
        for (i, lines) in pages.iter().enumerate() {
            let (page_id, content_id) = (page_ids[i], Ref::new(7 + 2 * i as i32));
            let mut page = pdf.page(page_id);
            page.media_box(Rect::new(0.0, 0.0, PAGE_W, PAGE_H)).parent(tree).contents(content_id);
            page.resources().fonts().pair(Name(b"F1"), regular).pair(Name(b"F2"), bold);
            page.finish();
            let mut c = Content::new();
            let mut y = PAGE_H - MARGIN;
            for l in lines {
                y -= l.size * 1.25;
                if l.rule {
                    c.set_line_width(0.5).move_to(MARGIN, y + 2.0).line_to(PAGE_W - MARGIN, y + 2.0).stroke();
                } else if !l.text.is_empty() {
                    let font = if l.style == Style::Bold { Name(b"F2") } else { Name(b"F1") };
                    c.begin_text().set_font(font, l.size).next_line(MARGIN, y).show(Str(&latin1(&l.text))).end_text();
                }
            }
            let footer = format!("{} - page {} of {n}", self.title, i + 1);
            c.begin_text()
                .set_font(Name(b"F1"), 7.0)
                .next_line(MARGIN, MARGIN - 12.0)
                .show(Str(&latin1(&footer)))
                .end_text();
            pdf.stream(content_id, &c.finish());
        }
        pdf.finish()
    }
}

/// Text as Windows-1252 bytes (Latin-1 range; anything else becomes `?`).
fn latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| if (c as u32) < 256 && !c.is_control() { c as u8 } else { b'?' }).collect()
}

/// Greedy word wrap; words longer than a line are cut.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        let indent: String = para.chars().take_while(|c| *c == ' ').collect();
        for word in para.split(' ').filter(|w| !w.is_empty()) {
            let mut word = word.to_string();
            while word.chars().count() > width {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                let head: String = word.chars().take(width).collect();
                word = word.chars().skip(width).collect();
                out.push(head);
            }
            let len = line.chars().count();
            if len > 0 && len + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut line));
            }
            if line.is_empty() {
                line = format!("{indent}{word}");
            } else {
                line.push(' ');
                line.push_str(&word);
            }
        }
        out.push(line);
    }
    out
}

/// `s` cut or padded to exactly `w` characters (cut text ends in `~`).
pub fn cell(s: &str, w: usize, right: bool) -> String {
    let n = s.chars().count();
    if n > w {
        let mut t: String = s.chars().take(w.saturating_sub(1)).collect();
        t.push('~');
        t
    } else if right {
        format!("{}{s}", " ".repeat(w - n))
    } else {
        format!("{s}{}", " ".repeat(w - n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_cuts() {
        assert_eq!(wrap("aaa bbb ccc", 7), ["aaa bbb", "ccc"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(cell("abcdef", 4, false), "abc~");
        assert_eq!(cell("ab", 4, true), "  ab");
    }

    #[test]
    fn renders_a_multi_page_pdf() {
        let mut d = Doc::new("Test");
        for i in 0..200 {
            d.text(&format!("line {i} café"), 8.0, if i % 10 == 0 { Style::Bold } else { Style::Normal });
        }
        d.rule();
        let bytes = d.render();
        assert!(bytes.starts_with(b"%PDF-"));
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("/Courier-Bold") && s.contains("/Count 4"), "pages: {}", s.matches("/Type /Page").count());
        // "line 7 café" in Windows-1252, hex-encoded by pdf-writer
        assert!(s.contains("<6C696E65203720636166E9>"));
    }
}
