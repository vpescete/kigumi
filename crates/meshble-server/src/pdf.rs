//! A pure-Rust PDF rasterizer (genpdf) for the report engine. It does NOT do full HTML layout: the
//! report templates are server-generated and structured (an `<h1>` title plus one `<table>` with a
//! header row and body/footer rows), so a small, dependency-free extractor pulls the heading + the table
//! cells out of that HTML and lays them out with genpdf. Short footer rows (Odoo-style colspan totals)
//! are right-padded so totals line up under the last column. The Liberation Sans family is embedded so
//! no font files need to ship or be discovered at runtime.

use crate::Rasterizer;
use genpdf::{elements, fonts, style, Document, Element, SimplePageDecorator};

const REGULAR: &[u8] = include_bytes!("../fonts/LiberationSans-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../fonts/LiberationSans-Bold.ttf");
const ITALIC: &[u8] = include_bytes!("../fonts/LiberationSans-Italic.ttf");
const BOLD_ITALIC: &[u8] = include_bytes!("../fonts/LiberationSans-BoldItalic.ttf");

/// Renders the report engine's structured HTML to a PDF with genpdf.
pub struct GenpdfRasterizer;

impl GenpdfRasterizer {
    pub fn new() -> Self {
        GenpdfRasterizer
    }

    fn font_family() -> Result<fonts::FontFamily<fonts::FontData>, String> {
        let load = |data: &[u8]| fonts::FontData::new(data.to_vec(), None).map_err(|e| e.to_string());
        Ok(fonts::FontFamily {
            regular: load(REGULAR)?,
            bold: load(BOLD)?,
            italic: load(ITALIC)?,
            bold_italic: load(BOLD_ITALIC)?,
        })
    }
}

impl Default for GenpdfRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Rasterizer for GenpdfRasterizer {
    fn render_pdf(&self, html: &str) -> Result<Vec<u8>, String> {
        let mut doc = Document::new(Self::font_family()?);
        let mut deco = SimplePageDecorator::new();
        deco.set_margins(12);
        doc.set_page_decorator(deco);

        let title = extract_first(html, "h1").unwrap_or_else(|| "Report".to_string());
        doc.set_title(&title);
        doc.push(elements::Paragraph::new(&title).styled(style::Style::new().bold().with_font_size(18)));
        doc.push(elements::Break::new(1));

        let rows = extract_table_rows(html);
        if !rows.is_empty() {
            let ncols = rows.iter().map(|r| r.cells.len()).max().unwrap_or(1).max(1);
            let mut table = elements::TableLayout::new(vec![1; ncols]);
            table.set_cell_decorator(elements::FrameCellDecorator::new(false, true, false));
            for r in &rows {
                // Right-pad short rows (colspan footer totals) so the value sits under the last column.
                let pad = ncols - r.cells.len();
                let mut row = table.row();
                for _ in 0..pad {
                    row = row.element(elements::Paragraph::new("").padded(1));
                }
                for c in &r.cells {
                    let p = elements::Paragraph::new(c).padded(1);
                    let styled = if r.header { p.styled(style::Style::new().bold()) } else { p.styled(style::Style::new()) };
                    row = row.element(styled);
                }
                row.push().map_err(|e| e.to_string())?;
            }
            doc.push(table);
        }

        let mut buf = Vec::new();
        doc.render(&mut buf).map_err(|e| e.to_string())?;
        Ok(buf)
    }
}

/// A parsed table row: its cell texts, and whether it is a header row (`<th>`).
struct HtmlRow {
    cells: Vec<String>,
    header: bool,
}

/// The text content of the first `<tag>…</tag>` element (inner tags stripped, entities decoded).
fn extract_first(html: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = html.find(&open)?;
    let after_open = html[start..].find('>')? + start + 1;
    let close = format!("</{tag}>");
    let end = html[after_open..].find(&close)? + after_open;
    Some(decode(&strip_tags(&html[after_open..end])))
}

/// Every `<tr>` in the HTML as an HtmlRow (cells from `<td>`/`<th>`; a row is a header iff it has a `<th>`).
fn extract_table_rows(html: &str) -> Vec<HtmlRow> {
    let mut rows = Vec::new();
    let mut rest = html;
    while let Some(s) = rest.find("<tr") {
        let after = &rest[s..];
        let Some(e) = after.find("</tr>") else { break };
        let row_html = &after[..e];
        let header = cell_texts(row_html, "th").len() > 0;
        let mut cells = cell_texts(row_html, "th");
        cells.extend(cell_texts(row_html, "td"));
        if !cells.is_empty() {
            rows.push(HtmlRow { cells, header });
        }
        rest = &after[e + 5..];
    }
    rows
}

/// The decoded text of every `<tag>…</tag>` cell in a row fragment, in order.
fn cell_texts(row_html: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = row_html;
    while let Some(s) = rest.find(&open) {
        let after = &rest[s..];
        let Some(gt) = after.find('>') else { break };
        let content_start = gt + 1;
        let Some(e) = after[content_start..].find(&close) else { break };
        let cell = &after[content_start..content_start + e];
        out.push(decode(&strip_tags(cell)));
        rest = &after[content_start + e + close.len()..];
    }
    out
}

/// Removes any `<...>` tags, leaving the text between them.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Decodes the HTML entities the report templates emit (the inverse of their escaping).
fn decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "<!doctype html><html><body><h1>Quotation SO/001</h1>\
        <table><thead><tr><th>Description</th><th class=\"r\">Qty</th><th class=\"r\">Subtotal</th></tr></thead>\
        <tbody><tr><td>Widget &amp; Co</td><td class=\"r\">2</td><td class=\"r\">200.00</td></tr></tbody>\
        <tfoot><tr><td colspan=\"2\" class=\"r\">Total</td><td class=\"r\">200.00</td></tr></tfoot></table></body></html>";

    #[test]
    fn extracts_heading_and_rows() {
        assert_eq!(extract_first(SAMPLE, "h1").as_deref(), Some("Quotation SO/001"));
        let rows = extract_table_rows(SAMPLE);
        assert_eq!(rows.len(), 3, "header + body + footer");
        assert!(rows[0].header, "the thead row is a header");
        assert_eq!(rows[0].cells, vec!["Description", "Qty", "Subtotal"]);
        assert_eq!(rows[1].cells, vec!["Widget & Co", "2", "200.00"], "entities decoded, tags stripped");
        assert_eq!(rows[2].cells, vec!["Total", "200.00"], "footer is short (colspan)");
    }

    #[test]
    fn renders_a_nonempty_pdf() {
        let bytes = GenpdfRasterizer::new().render_pdf(SAMPLE).expect("render");
        assert!(bytes.starts_with(b"%PDF"), "output is a PDF");
        assert!(bytes.len() > 1000, "non-trivial PDF ({} bytes)", bytes.len());
    }
}
