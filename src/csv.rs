//! CSV and TSV files, shown as a table and written back as delimited text.
//!
//! Pure functions, kept free of GTK so they can be unit tested. A file the
//! table cannot represent faithfully (a field spanning several lines, an
//! unclosed quote, too many cells) stays plain text.

use std::path::Path;

use crate::model::{self, Align, Block, Line, Span, Style, Table};

/// Beyond this many cells, the table view (one widget per cell) gets slow.
const MAX_CELLS: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub delim: char,
    pub crlf: bool,
    pub final_newline: bool,
}

/// What a CSV tab remembers from the text it was opened with.
#[derive(Clone, Debug)]
pub struct Sheet {
    pub format: Format,
    /// Rows as the table showed them right after loading.
    pub baseline: Vec<Vec<String>>,
    /// Text the table was built from, re-emitted while nothing changed.
    pub text: String,
}

/// Whether `p` is a delimited file; `Some(None)` when the delimiter must be
/// guessed from the content.
pub fn kind(p: &Path) -> Option<Option<char>> {
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("csv") => Some(None),
        Some("tsv" | "tab") => Some(Some('\t')),
        _ => None,
    }
}

pub fn is_csv_path(p: &Path) -> bool {
    kind(p).is_some()
}

/// The most frequent candidate delimiter on the first line, outside quotes.
pub fn guess_delimiter(text: &str) -> char {
    let mut counts = [(',', 0usize), (';', 0), ('\t', 0), ('|', 0)];
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            '\n' if !quoted => break,
            _ if !quoted => {
                if let Some(e) = counts.iter_mut().find(|(d, _)| *d == c) {
                    e.1 += 1;
                }
            }
            _ => {}
        }
    }
    // Ties keep the earlier candidate; no candidate at all means a comma.
    counts.iter().fold((',', 0), |best, &(d, n)| if n > best.1 { (d, n) } else { best }).0
}

pub fn format_of(text: &str, delim: char) -> Format {
    Format { delim, crlf: text.contains("\r\n"), final_newline: text.ends_with('\n') }
}

/// Parses RFC 4180 text. None if a quote is never closed.
pub fn parse(text: &str, delim: char) -> Option<Vec<Vec<String>>> {
    let body = text.strip_suffix('\n').map(|t| t.strip_suffix('\r').unwrap_or(t)).unwrap_or(text);
    if body.is_empty() {
        return Some(Vec::new());
    }
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = body.chars().peekable();
    let mut quoted = false;
    // A field that began with a quote: its quotes are syntax, not text.
    let mut at_start = true;
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if at_start => {
                quoted = true;
                at_start = false;
            }
            c if c == delim => {
                row.push(std::mem::take(&mut field));
                at_start = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                at_start = true;
            }
            c => {
                field.push(c);
                at_start = false;
            }
        }
    }
    if quoted {
        return None;
    }
    row.push(field);
    rows.push(row);
    Some(rows)
}

fn quote(field: &str, delim: char) -> String {
    if field.contains([delim, '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

pub fn write(rows: &[Vec<String>], f: Format) -> String {
    let eol = if f.crlf { "\r\n" } else { "\n" };
    let mut out = rows
        .iter()
        .map(|r| r.iter().map(|c| quote(c, f.delim)).collect::<Vec<_>>().join(&f.delim.to_string()))
        .collect::<Vec<_>>()
        .join(eol);
    if f.final_newline && !rows.is_empty() {
        out.push_str(eol);
    }
    out
}

/// Inline Markdown that shows `text` literally.
fn literal(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    model::serialize_line(&Line { block: Block::Para, spans: vec![Span { text: text.to_string(), style: Style::default() }] })
}

fn plain(md: &str) -> String {
    model::parse_inline(md).into_iter().map(|s| s.text).collect()
}

/// The text as a Markdown table, or None when a table would not show it
/// faithfully.
pub fn to_markdown(text: &str, delim: char) -> Option<String> {
    let rows = parse(text, delim)?;
    let cols = rows.iter().map(Vec::len).max()?;
    if rows.len() * cols > MAX_CELLS || rows.iter().flatten().any(|c| c.contains(['\n', '\r'])) {
        return None;
    }
    let cell_row = |r: &Vec<String>| {
        let mut v: Vec<String> = r.iter().map(|c| literal(c)).collect();
        v.resize(cols, String::new());
        v
    };
    let table = Table {
        header: cell_row(&rows[0]),
        align: vec![Align::None; cols],
        rows: rows[1..].iter().map(cell_row).collect(),
        raw: None,
        wide: false,
    };
    Some(model::serialize_table(&table))
}

/// The rows a document shows: table rows, then any other non-empty line as a
/// one-field row so that nothing typed is lost.
pub fn rows_of(lines: &[Line]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for line in lines {
        match &line.block {
            Block::Table(t) => {
                rows.push(t.header.iter().map(|c| plain(c)).collect());
                rows.extend(t.rows.iter().map(|r| r.iter().map(|c| plain(c)).collect()));
            }
            _ => {
                let text = line.text();
                if !text.trim().is_empty() {
                    rows.push(vec![text]);
                }
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(text: &str, delim: char) -> Vec<Vec<String>> {
        rows_of(&model::parse(&to_markdown(text, delim).unwrap()))
    }

    #[test]
    fn parses_quotes_and_delimiters() {
        let rows = parse("a,\"b,c\",\"say \"\"hi\"\"\"\r\n1,2,3\r\n", ',').unwrap();
        assert_eq!(rows, vec![vec!["a", "b,c", "say \"hi\""], vec!["1", "2", "3"]]);
        assert_eq!(parse("a,\"open", ','), None);
        assert_eq!(parse("", ','), Some(vec![]));
    }

    #[test]
    fn guesses_delimiter() {
        assert_eq!(guess_delimiter("a;b;c\n1,5;2;3"), ';');
        assert_eq!(guess_delimiter("a\tb\n"), '\t');
        assert_eq!(guess_delimiter("\"x;y\",b\n"), ',');
        assert_eq!(guess_delimiter("single"), ',');
    }

    #[test]
    fn writes_back() {
        let f = format_of("a\r\n", ',');
        let rows = vec![vec!["a,b".to_string(), "q\"".into()], vec!["1".into(), "".into()]];
        assert_eq!(write(&rows, f), "\"a,b\",\"q\"\"\"\r\n1,\r\n");
        assert_eq!(parse(&write(&rows, f), ',').unwrap(), rows);
    }

    #[test]
    fn markdown_characters_stay_literal() {
        let text = "name,note\n*a*,x|y\n# b,`c` [d](e) \\ _f_\n";
        assert_eq!(roundtrip(text, ','), parse(text, ',').unwrap());
    }

    #[test]
    fn ragged_rows_are_padded() {
        assert_eq!(roundtrip("a,b,c\n1\n", ','), vec![vec!["a", "b", "c"], vec!["1", "", ""]]);
    }

    #[test]
    fn multiline_fields_stay_plain() {
        assert_eq!(to_markdown("a,b\n\"x\ny\",z\n", ','), None);
    }

    #[test]
    fn other_lines_become_rows() {
        let mut lines = model::parse(&to_markdown("a,b\n1,2", ',').unwrap());
        lines.push(Line::plain(Block::Para, ""));
        lines.push(Line::plain(Block::Para, "note"));
        assert_eq!(rows_of(&lines), vec![vec!["a", "b"], vec!["1", "2"], vec!["note"]]);
    }
}
