use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::Deserialize;
use std::fmt;
use unicode_width::UnicodeWidthStr;

/// How to render markdown tables for a given channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TableMode {
    /// Wrap the table in a fenced code block (default).
    Code,
    /// Convert each row into bullet points.
    Bullets,
    /// Pass through unchanged.
    Off,
}

impl Default for TableMode {
    fn default() -> Self {
        Self::Code
    }
}

impl fmt::Display for TableMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code => write!(f, "code"),
            Self::Bullets => write!(f, "bullets"),
            Self::Off => write!(f, "off"),
        }
    }
}

// ── IR types ────────────────────────────────────────────────────────

/// A parsed table: header row + data rows, each cell is plain text.
struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

/// Segment of the document — either verbatim text or a parsed table.
enum Segment {
    Text(String),
    Table(Table),
}

// ── Public API ──────────────────────────────────────────────────────

/// Parse markdown, detect tables via pulldown-cmark, and render them
/// according to `mode`. Non-table content passes through unchanged.
pub fn convert_tables(markdown: &str, mode: TableMode) -> String {
    if mode == TableMode::Off || markdown.is_empty() {
        return markdown.to_string();
    }

    let segments = parse_segments(markdown, mode);

    let mut out = String::with_capacity(markdown.len());
    for seg in segments {
        match seg {
            Segment::Text(t) => out.push_str(&t),
            Segment::Table(table) => match mode {
                TableMode::Code => render_table_code(&table, &mut out),
                TableMode::Bullets => render_table_bullets(&table, &mut out),
                TableMode::Off => unreachable!(),
            },
        }
    }
    out
}

// ── Parser ──────────────────────────────────────────────────────────

/// Walk the markdown source with pulldown-cmark and split it into
/// text segments and parsed Table segments.
fn parse_segments(markdown: &str, mode: TableMode) -> Vec<Segment> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);

    let mut segments: Vec<Segment> = Vec::new();
    let mut in_table = false;
    let mut in_head = false;
    let mut headers: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut cell_buf = String::new();
    let mut last_table_end: usize = 0;

    // We need byte offsets to grab non-table text verbatim.
    let parser_with_offsets = Parser::new_ext(markdown, opts).into_offset_iter();

    for (event, range) in parser_with_offsets {
        match event {
            Event::Start(Tag::Table(_)) => {
                // Flush text before this table
                let before = &markdown[last_table_end..range.start];
                if !before.is_empty() {
                    push_text(&mut segments, before);
                }
                in_table = true;
                headers.clear();
                rows.clear();
            }
            Event::End(TagEnd::Table) => {
                let table = Table {
                    headers: std::mem::take(&mut headers),
                    rows: std::mem::take(&mut rows),
                };
                segments.push(Segment::Table(table));
                in_table = false;
                last_table_end = range.end;
            }
            Event::Start(Tag::TableHead) => {
                in_head = true;
                current_row.clear();
            }
            Event::End(TagEnd::TableHead) => {
                headers = std::mem::take(&mut current_row);
                in_head = false;
            }
            Event::Start(Tag::TableRow) => {
                current_row.clear();
            }
            Event::End(TagEnd::TableRow) => {
                if !in_head {
                    rows.push(std::mem::take(&mut current_row));
                }
            }
            Event::Start(Tag::TableCell) => {
                cell_buf.clear();
            }
            Event::End(TagEnd::TableCell) => {
                current_row.push(cell_buf.trim().to_string());
                cell_buf.clear();
            }
            Event::Text(t) if in_table => {
                cell_buf.push_str(&t);
            }
            Event::Code(t) if in_table => {
                // In Code mode the table is already inside a fenced code block,
                // so backticks would render as literal characters. Strip them.
                if mode != TableMode::Code {
                    cell_buf.push('`');
                }
                cell_buf.push_str(&t);
                if mode != TableMode::Code {
                    cell_buf.push('`');
                }
            }
            // Inline markup inside cells: collect text, ignore tags
            Event::SoftBreak if in_table => {
                cell_buf.push(' ');
            }
            Event::HardBreak if in_table => {
                cell_buf.push(' ');
            }
            // Start/End of inline tags (bold, italic, link, etc.) — skip the
            // tag markers but keep processing their child text events above.
            Event::Start(Tag::Emphasis)
            | Event::Start(Tag::Strong)
            | Event::Start(Tag::Strikethrough)
            | Event::Start(Tag::Link { .. })
            | Event::End(TagEnd::Emphasis)
            | Event::End(TagEnd::Strong)
            | Event::End(TagEnd::Strikethrough)
            | Event::End(TagEnd::Link)
                if in_table => {}
            _ => {}
        }
    }

    // Remaining text after last table
    if last_table_end < markdown.len() {
        let tail = &markdown[last_table_end..];
        if !tail.is_empty() {
            push_text(&mut segments, tail);
        }
    }

    segments
}

fn push_text(segments: &mut Vec<Segment>, text: &str) {
    if let Some(Segment::Text(ref mut prev)) = segments.last_mut() {
        prev.push_str(text);
    } else {
        segments.push(Segment::Text(text.to_string()));
    }
}

// ── Renderers ───────────────────────────────────────────────────────

/// Render table as a fenced code block with aligned columns.
fn render_table_code(table: &Table, out: &mut String) {
    let col_count = table
        .headers
        .len()
        .max(table.rows.iter().map(|r| r.len()).max().unwrap_or(0));
    if col_count == 0 {
        return;
    }

    // Compute column widths (using display width for CJK/emoji)
    let mut widths = vec![0usize; col_count];
    for (i, h) in table.headers.iter().enumerate() {
        widths[i] = widths[i].max(UnicodeWidthStr::width(h.as_str()));
    }
    for row in &table.rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_count {
                widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
            }
        }
    }
    // Minimum width 3 for the divider
    for w in &mut widths {
        *w = (*w).max(3);
    }

    out.push_str("```\n");

    // Header row
    write_row(out, &table.headers, &widths, col_count);
    // Divider
    out.push('|');
    for w in &widths {
        out.push(' ');
        for _ in 0..*w {
            out.push('-');
        }
        out.push_str(" |");
    }
    out.push('\n');
    // Data rows
    for row in &table.rows {
        write_row(out, row, &widths, col_count);
    }

    out.push_str("```\n");
}

fn write_row(out: &mut String, cells: &[String], widths: &[usize], col_count: usize) {
    out.push('|');
    for i in 0..col_count {
        out.push(' ');
        let cell = cells.get(i).map(|s| s.as_str()).unwrap_or("");
        out.push_str(cell);
        let display_width = UnicodeWidthStr::width(cell);
        let pad = widths[i].saturating_sub(display_width);
        for _ in 0..pad {
            out.push(' ');
        }
        out.push_str(" |");
    }
    out.push('\n');
}

/// Render table as bullet points: `• header: value` per cell.
fn render_table_bullets(table: &Table, out: &mut String) {
    for (row_idx, row) in table.rows.iter().enumerate() {
        for (i, cell) in row.iter().enumerate() {
            if cell.is_empty() {
                continue;
            }
            out.push_str("• ");
            if let Some(h) = table.headers.get(i) {
                if !h.is_empty() {
                    out.push_str(h);
                    out.push_str(": ");
                }
            }
            out.push_str(cell);
            out.push('\n');
        }
        // Blank line between rows, but not after the last one
        if row_idx + 1 < table.rows.len() {
            out.push('\n');
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE_MD: &str = "\
Some text before.

| Name  | Age |
|-------|-----|
| Alice | 30  |
| Bob   | 25  |

Some text after.
";

    #[test]
    fn off_mode_passes_through() {
        let result = convert_tables(TABLE_MD, TableMode::Off);
        assert_eq!(result, TABLE_MD);
    }

    #[test]
    fn code_mode_wraps_in_codeblock() {
        let result = convert_tables(TABLE_MD, TableMode::Code);
        assert!(result.contains("```\n"));
        assert!(result.contains("| Alice"));
        assert!(result.contains("Some text before."));
        assert!(result.contains("Some text after."));
    }

    #[test]
    fn bullets_mode_converts_to_bullets() {
        let result = convert_tables(TABLE_MD, TableMode::Bullets);
        assert!(result.contains("• Name: Alice"));
        assert!(result.contains("• Age: 30"));
        assert!(!result.contains("```"));
    }

    #[test]
    fn no_table_passes_through() {
        let plain = "Hello world\nNo tables here.";
        let result = convert_tables(plain, TableMode::Code);
        assert_eq!(result, plain);
    }

    #[test]
    fn code_mode_strips_backticks_from_code_cells() {
        let md = "| col |\n|-----|\n| `value` |\n";
        let result = convert_tables(md, TableMode::Code);
        // The table is inside a ``` block — backtick wrapping must be stripped.
        assert!(result.contains("value"), "cell content should be present");
        // Only the fence markers themselves should contain backticks.
        let inner = result
            .trim_start_matches("```\n")
            .trim_end_matches("```\n");
        assert!(
            !inner.contains('`'),
            "no backticks should appear inside the code fence: {result:?}"
        );
    }

    #[test]
    fn bullets_mode_keeps_backticks_in_code_cells() {
        let md = "| col |\n|-----|\n| `value` |\n";
        let result = convert_tables(md, TableMode::Bullets);
        assert!(result.contains("`value`"), "backticks should be kept in bullets mode");
    }
}
