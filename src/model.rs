//! Pure Markdown document model: parsing and serialisation.
//!
//! The editor never shows Markdown syntax. A document is a list of lines, each
//! with a block kind and styled spans. This module converts between that model
//! and Markdown text, and is kept free of GTK so it can be unit tested.

#[derive(Clone, Debug, PartialEq, Eq, Default, Hash)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Para,
    Heading(u8),
    Quote,
    Bullet { indent: String, marker: char },
    Numbered { indent: String, marker: String },
    Check { indent: String, marker: char, done: bool },
    /// A line inside a fenced code block, kept verbatim.
    Code,
    /// A fence line (```lang), kept verbatim.
    Fence,
    /// A GitHub-style table; it stands for several Markdown lines.
    Table(Table),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// Cell texts (inline Markdown, with `\|` unescaped).
    pub header: Vec<String>,
    pub align: Vec<Align>,
    pub rows: Vec<Vec<String>>,
    /// Original source, re-emitted untouched until the table is edited.
    pub raw: Option<String>,
    /// Display only: may grow past the text column (not in Markdown).
    pub wide: bool,
}

impl Table {
    pub fn new(cols: usize, rows: usize) -> Table {
        Table {
            header: (1..=cols).map(|i| format!("Colonne {i}")).collect(),
            align: vec![Align::None; cols],
            rows: vec![vec![String::new(); cols]; rows],
            raw: None,
            wide: false,
        }
    }

    pub fn cols(&self) -> usize {
        self.header.len()
    }
}

/// Splits a table row into cells, honouring `\|` escapes.
pub fn split_cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = if t.ends_with('|') && !t.ends_with("\\|") { &t[..t.len() - 1] } else { t };
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cur.push('|');
            chars.next();
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

pub fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.contains('|') && !t.is_empty()
}

fn delimiter(line: &str) -> Option<Vec<Align>> {
    if !line.contains('-') || !is_table_row(line) && !line.trim().starts_with('-') {
        return None;
    }
    let cells = split_cells(line);
    let mut out = Vec::new();
    for c in cells {
        let left = c.starts_with(':');
        let right = c.ends_with(':');
        let dashes = c.trim_matches(':');
        if dashes.is_empty() || !dashes.chars().all(|ch| ch == '-') {
            return None;
        }
        out.push(match (left, right) {
            (true, true) => Align::Center,
            (true, false) => Align::Left,
            (false, true) => Align::Right,
            _ => Align::None,
        });
    }
    Some(out)
}

/// A table from its header and delimiter lines, if they form one.
pub fn table_head(header: &str, delim: &str) -> Option<Table> {
    if !is_table_row(header) {
        return None;
    }
    let align = delimiter(delim)?;
    let header = split_cells(header);
    if header.len() != align.len() {
        return None;
    }
    Some(Table { header, align, rows: Vec::new(), raw: None, wide: false })
}

fn escape_cell(s: &str) -> String {
    let mut out = String::new();
    let mut prev = '\0';
    for c in s.chars() {
        if c == '|' && prev != '\\' {
            out.push('\\');
        }
        out.push(c);
        prev = c;
    }
    out
}

pub fn serialize_table(t: &Table) -> String {
    if let Some(raw) = &t.raw {
        return raw.clone();
    }
    let cols = t.cols().max(1);
    let cell = |row: &[String], i: usize| escape_cell(row.get(i).map(String::as_str).unwrap_or(""));
    let mut widths = vec![3usize; cols];
    for row in std::iter::once(&t.header).chain(t.rows.iter()) {
        for (i, w) in widths.iter_mut().enumerate() {
            *w = (*w).max(cell(row, i).chars().count());
        }
    }
    let line = |row: &[String]| {
        let mut s = String::from("|");
        for (i, w) in widths.iter().enumerate() {
            let c = cell(row, i);
            let pad = w - c.chars().count();
            s.push(' ');
            s.push_str(&c);
            s.push_str(&" ".repeat(pad));
            s.push_str(" |");
        }
        s
    };
    let mut out = vec![line(&t.header)];
    let mut d = String::from("|");
    for (i, w) in widths.iter().enumerate() {
        let a = t.align.get(i).copied().unwrap_or(Align::None);
        let (l, r) = match a {
            Align::None => ("", ""),
            Align::Left => (":", ""),
            Align::Right => ("", ":"),
            Align::Center => (":", ":"),
        };
        let n = w - l.len() - r.len();
        d.push_str(&format!(" {l}{}{r} |", "-".repeat(n)));
    }
    out.push(d);
    for row in &t.rows {
        out.push(line(row));
    }
    out.join("\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub block: Block,
    pub spans: Vec<Span>,
}

impl Line {
    pub fn plain(block: Block, text: &str) -> Line {
        let spans = if text.is_empty() {
            Vec::new()
        } else {
            vec![Span { text: text.to_string(), style: Style::default() }]
        };
        Line { block, spans }
    }

    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

pub fn parse(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut fence: Option<String> = None;
    let raws: Vec<&str> = text.split('\n').map(|r| r.strip_suffix('\r').unwrap_or(r)).collect();
    let mut i = 0;
    while i < raws.len() {
        let raw = raws[i];
        i += 1;
        let trimmed = raw.trim_start();
        if let Some(open) = &fence {
            if trimmed.starts_with(open.as_str()) && trimmed[open.len()..].trim().is_empty() {
                fence = None;
                lines.push(Line::plain(Block::Fence, raw));
            } else {
                lines.push(Line::plain(Block::Code, raw));
            }
            continue;
        }
        if let Some(f) = fence_marker(trimmed) {
            fence = Some(f);
            lines.push(Line::plain(Block::Fence, raw));
            continue;
        }
        if i < raws.len()
            && let Some(mut table) = table_head(raw, raws[i])
        {
            let start = i - 1;
            i += 1;
            while i < raws.len() && is_table_row(raws[i]) && fence_marker(raws[i].trim_start()).is_none() {
                let mut row = split_cells(raws[i]);
                row.resize(table.cols(), String::new());
                table.rows.push(row);
                i += 1;
            }
            table.raw = Some(raws[start..i].join("\n"));
            lines.push(Line { block: Block::Table(table), spans: Vec::new() });
            continue;
        }
        lines.push(parse_block_line(raw));
    }
    lines
}

fn fence_marker(trimmed: &str) -> Option<String> {
    for ch in ['`', '~'] {
        let n = trimmed.chars().take_while(|&c| c == ch).count();
        if n >= 3 {
            if ch == '`' && trimmed[n..].contains('`') {
                return None;
            }
            return Some(std::iter::repeat_n(ch, n).collect());
        }
    }
    None
}

/// Splits a line into its block kind and the remaining inline content.
pub fn split_block(raw: &str) -> (Block, &str) {
    // Headings
    let hashes = raw.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &raw[hashes..];
        if let Some(content) = rest.strip_prefix(' ') {
            return (Block::Heading(hashes as u8), content);
        }
    }
    // Quotes
    if let Some(rest) = raw.strip_prefix('>') {
        return (Block::Quote, rest.strip_prefix(' ').unwrap_or(rest));
    }
    // Lists
    let indent_len = raw.len() - raw.trim_start_matches([' ', '\t']).len();
    let indent = &raw[..indent_len];
    let rest = &raw[indent_len..];
    let mut chars = rest.chars();
    if let Some(m @ ('-' | '*' | '+')) = chars.next() {
        let after = &rest[1..];
        if after == " [ ]" || after == " [x]" || after == " [X]" {
            return (
                Block::Check { indent: indent.into(), marker: m, done: after != " [ ]" },
                "",
            );
        }
        for (pat, done) in [(" [ ] ", false), (" [x] ", true), (" [X] ", true)] {
            if let Some(content) = after.strip_prefix(pat) {
                return (Block::Check { indent: indent.into(), marker: m, done }, content);
            }
        }
        if let Some(content) = after.strip_prefix(' ') {
            return (Block::Bullet { indent: indent.into(), marker: m }, content);
        }
    }
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits) {
        let after = &rest[digits..];
        if let Some(delim @ ('.' | ')')) = after.chars().next()
            && let Some(content) = after[1..].strip_prefix(' ')
        {
            let marker = format!("{}{}", &rest[..digits], delim);
            return (Block::Numbered { indent: indent.into(), marker }, content);
        }
    }
    (Block::Para, raw)
}

fn parse_block_line(raw: &str) -> Line {
    let (block, content) = split_block(raw);
    Line { block, spans: parse_inline(content) }
}

pub fn parse_inline(s: &str) -> Vec<Span> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    inline_into(&chars, &Style::default(), &mut out);
    merge(out)
}

fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation()
}

fn find_seq(chars: &[char], from: usize, pat: &[char]) -> Option<usize> {
    let mut i = from;
    while i + pat.len() <= chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i..i + pat.len()] == *pat {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn word_char(c: Option<&char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric())
}

fn valid_content(content: &[char]) -> bool {
    !content.is_empty()
        && !content[0].is_whitespace()
        && !content[content.len() - 1].is_whitespace()
}

/// Finds a closing delimiter for an emphasis opener of `n` copies of `d`.
fn find_closer(chars: &[char], from: usize, d: char, n: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            i += 2;
            continue;
        }
        if c == '`' {
            // Skip inline code spans entirely.
            if let Some(j) = find_seq(chars, i + 1, &['`']) {
                i = j + 1;
                continue;
            }
        }
        if c == d {
            let run = chars[i..].iter().take_while(|&&x| x == d).count();
            if run >= n {
                // Prefer the closer at the end of a longer run (e.g. `***`).
                let at = i + run - n;
                if valid_content(&chars[from..at])
                    && (d != '_' || !word_char(chars.get(at + n)))
                {
                    return Some(at);
                }
            }
            i += run;
            continue;
        }
        i += 1;
    }
    None
}

fn push_text(out: &mut Vec<Span>, text: &str, style: &Style) {
    if text.is_empty() {
        return;
    }
    out.push(Span { text: text.to_string(), style: style.clone() });
}

fn inline_into(chars: &[char], style: &Style, out: &mut Vec<Span>) {
    let mut buf = String::new();
    let mut i = 0;
    let flush = |buf: &mut String, out: &mut Vec<Span>| {
        push_text(out, buf, style);
        buf.clear();
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && is_punct(chars[i + 1]) {
            buf.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '`' && !style.code
            && let Some(j) = find_seq(chars, i + 1, &['`'])
                && j > i + 1
            {
                flush(&mut buf, out);
                let mut st = style.clone();
                st.code = true;
                let text: String = chars[i + 1..j].iter().collect();
                push_text(out, &text, &st);
                i = j + 1;
                continue;
            }
        if (c == '*' || c == '_') && !style.code {
            let run = chars[i..].iter().take_while(|&&x| x == c).count();
            let left_ok = c != '_' || !word_char(if i > 0 { chars.get(i - 1) } else { None });
            if left_ok {
                // Try strong+emphasis, strong, emphasis in order.
                let mut handled = false;
                for n in [3usize, 2, 1] {
                    if run < n {
                        continue;
                    }
                    let start = i + n;
                    if let Some(j) = find_closer(chars, start, c, n) {
                        flush(&mut buf, out);
                        let mut st = style.clone();
                        if n >= 2 {
                            st.bold = true;
                        }
                        if n != 2 {
                            st.italic = true;
                        }
                        // Any extra opener characters stay literal.
                        inline_into(&chars[start..j], &st, out);
                        i = j + n;
                        handled = true;
                        break;
                    }
                }
                if handled {
                    continue;
                }
            }
            for _ in 0..run {
                buf.push(c);
            }
            i += run;
            continue;
        }
        if c == '~' && !style.code && chars.get(i + 1) == Some(&'~')
            && let Some(j) = find_seq(chars, i + 2, &['~', '~'])
                && valid_content(&chars[i + 2..j])
            {
                flush(&mut buf, out);
                let mut st = style.clone();
                st.strike = true;
                inline_into(&chars[i + 2..j], &st, out);
                i = j + 2;
                continue;
            }
        if c == '[' && style.link.is_none() && !style.code && !(i > 0 && chars[i - 1] == '!')
            && let Some((text_end, url, end)) = match_link(chars, i) {
                flush(&mut buf, out);
                let mut st = style.clone();
                st.link = Some(url);
                inline_into(&chars[i + 1..text_end], &st, out);
                i = end;
                continue;
            }
        if (c == 'h') && style.link.is_none() && !style.code && !word_char(if i > 0 { chars.get(i - 1) } else { None })
            && let Some(end) = match_autolink(chars, i) {
                flush(&mut buf, out);
                let url: String = chars[i..end].iter().collect();
                let mut st = style.clone();
                st.link = Some(url.clone());
                push_text(out, &url, &st);
                i = end;
                continue;
            }
        buf.push(c);
        i += 1;
    }
    flush(&mut buf, out);
}

/// `[text](url)` starting at `i`. Returns (index of `]`, url, index after `)`).
fn match_link(chars: &[char], i: usize) -> Option<(usize, String, usize)> {
    let mut depth = 0;
    let mut j = i + 1;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 1,
            '[' => depth += 1,
            ']' if depth == 0 => break,
            ']' => depth -= 1,
            _ => {}
        }
        j += 1;
    }
    if j >= chars.len() || j == i + 1 || chars.get(j + 1) != Some(&'(') {
        return None;
    }
    let url_start = j + 2;
    let mut k = url_start;
    while k < chars.len() && chars[k] != ')' {
        if chars[k].is_whitespace() {
            return None;
        }
        k += 1;
    }
    if k >= chars.len() || k == url_start {
        return None;
    }
    Some((j, chars[url_start..k].iter().collect(), k + 1))
}

pub fn match_autolink(chars: &[char], i: usize) -> Option<usize> {
    let rest = &chars[i..];
    let prefix_len = if rest.starts_with(&['h', 't', 't', 'p', 's', ':', '/', '/']) {
        8
    } else if rest.starts_with(&['h', 't', 't', 'p', ':', '/', '/']) {
        7
    } else {
        return None;
    };
    let mut end = i + prefix_len;
    while end < chars.len() && !chars[end].is_whitespace() && !matches!(chars[end], '<' | '>' | '"' | '`') {
        end += 1;
    }
    while end > i + prefix_len && matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '*' | '_' | '~' | '\'') {
        end -= 1;
    }
    (end > i + prefix_len).then_some(end)
}

fn merge(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for s in spans {
        if let Some(last) = out.last_mut()
            && last.style == s.style
        {
            last.text.push_str(&s.text);
            continue;
        }
        out.push(s);
    }
    out
}

// ---------------------------------------------------------------------------
// Serialisation
// ---------------------------------------------------------------------------

pub fn serialize(lines: &[Line]) -> String {
    let mut out = String::new();
    for (n, line) in lines.iter().enumerate() {
        if n > 0 {
            out.push('\n');
            // Other renderers would read a line right after a table as a row.
            if matches!(lines[n - 1].block, Block::Table(_)) && !(line.block == Block::Para && line.spans.is_empty()) {
                out.push('\n');
            }
        }
        out.push_str(&serialize_line(line));
    }
    out
}

pub fn serialize_line(line: &Line) -> String {
    let prefix = match &line.block {
        Block::Code | Block::Fence => return line.text(),
        Block::Table(t) => return serialize_table(t),
        Block::Para => String::new(),
        Block::Heading(n) => format!("{} ", "#".repeat(*n as usize)),
        Block::Quote => "> ".into(),
        Block::Bullet { indent, marker } => format!("{indent}{marker} "),
        Block::Numbered { indent, marker } => format!("{indent}{marker} "),
        Block::Check { indent, marker, done } => {
            format!("{indent}{marker} [{}] ", if *done { 'x' } else { ' ' })
        }
    };
    if line.spans.is_empty() {
        return match &line.block {
            Block::Quote => ">".into(),
            Block::Check { .. } => prefix.trim_end().to_string(),
            _ => prefix,
        };
    }
    let expected = Line { block: line.block.clone(), spans: merge(line.spans.clone()) };
    // Text that would open a code fence must stay literal.
    let raw = expected.text();
    let fence_like = matches!(line.block, Block::Para) && fence_marker(raw.trim_start()).is_some();
    let mut last = String::new();
    for (escape, alt) in [(false, 0), (false, 1), (false, 2), (true, 0), (true, 1), (true, 2)] {
        let body = serialize_spans(&expected.spans, escape, alt, matches!(line.block, Block::Para));
        let mut candidate = format!("{prefix}{body}");
        if fence_like {
            let lead = candidate.len() - candidate.trim_start().len();
            candidate.insert(lead, '\\');
        }
        if parse_block_line(&candidate) == expected {
            return candidate;
        }
        last = candidate;
    }
    last
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mark {
    Strike,
    Bold,
    Italic,
}

/// `alt` picks the delimiter flavour: 0 = `**`/`*`, 1 = `**`/`_`, 2 = `__`/`*`.
fn mark_str(m: Mark, alt: u8) -> &'static str {
    match m {
        Mark::Strike => "~~",
        Mark::Bold if alt == 2 => "__",
        Mark::Bold => "**",
        Mark::Italic if alt == 1 => "_",
        Mark::Italic => "*",
    }
}

fn wanted(style: &Style) -> Vec<Mark> {
    let mut v = Vec::new();
    if style.strike {
        v.push(Mark::Strike);
    }
    if style.bold {
        v.push(Mark::Bold);
    }
    if style.italic {
        v.push(Mark::Italic);
    }
    v
}

fn escape_text(text: &str, out: &mut String, at_line_start: bool) {
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let first = at_line_start && i == 0;
        let needs = matches!(c, '\\' | '*' | '_' | '`' | '~' | '[' | ']')
            || (first && matches!(c, '#' | '>' | '-' | '+'))
            || (c == ':' && i >= 4 && {
                let before: String = chars[i.saturating_sub(5)..i].iter().collect();
                before.ends_with("http") || before.ends_with("https")
            });
        if needs {
            out.push('\\');
        }
        out.push(c);
        // "1. " at line start: escape the delimiter.
        if at_line_start
            && i + 1 < chars.len()
            && chars[..=i].iter().all(|c| c.is_ascii_digit())
            && matches!(chars[i + 1], '.' | ')')
        {
            out.push('\\');
        }
    }
}

fn serialize_spans(spans: &[Span], escape: bool, alt: u8, para: bool) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < spans.len() {
        if let Some(url) = &spans[i].style.link {
            let mut j = i;
            while j < spans.len() && spans[j].style.link.as_ref() == Some(url) {
                j += 1;
            }
            let inner: Vec<Span> = spans[i..j]
                .iter()
                .map(|s| Span { text: s.text.clone(), style: Style { link: None, ..s.style.clone() } })
                .collect();
            let text: String = inner.iter().map(|s| s.text.as_str()).collect();
            let plain = inner.iter().all(|s| s.style == Style::default());
            if plain && text == *url {
                out.push_str(url);
            } else {
                out.push('[');
                out.push_str(&serialize_plain_runs(&inner, escape, alt, false));
                out.push_str("](");
                out.push_str(url);
                out.push(')');
            }
            i = j;
        } else {
            let mut j = i;
            while j < spans.len() && spans[j].style.link.is_none() {
                j += 1;
            }
            let at_start = para && out.is_empty();
            out.push_str(&serialize_plain_runs(&spans[i..j], escape, alt, at_start));
            i = j;
        }
    }
    out
}

fn serialize_plain_runs(spans: &[Span], escape: bool, alt: u8, line_start: bool) -> String {
    let mut out = String::new();
    let mut stack: Vec<Mark> = Vec::new();
    for span in spans {
        let want = wanted(&span.style);
        // Close everything above the lowest mark that is no longer wanted.
        if let Some(pos) = stack.iter().position(|m| !want.contains(m)) {
            while stack.len() > pos {
                let m = stack.pop().unwrap();
                out.push_str(mark_str(m, alt));
            }
        }
        for m in want {
            if !stack.contains(&m) {
                out.push_str(mark_str(m, alt));
                stack.push(m);
            }
        }
        if span.style.code {
            out.push('`');
            out.push_str(&span.text);
            out.push('`');
        } else if escape {
            let at = line_start && out.is_empty();
            escape_text(&span.text, &mut out, at);
        } else {
            out.push_str(&span.text);
        }
    }
    while let Some(m) = stack.pop() {
        out.push_str(mark_str(m, alt));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(s: &str) {
        let lines = parse(s);
        let out = serialize(&lines);
        assert_eq!(out, s, "round trip changed text; model = {lines:#?}");
        assert_eq!(parse(&out), lines);
    }

    #[test]
    fn round_trips() {
        rt("# Titre\n\nUn **gras** et *italique* et ~~barré~~ et `code`.");
        rt("- a\n- b\n  - c\n* d\n+ e");
        rt("1. un\n2. deux\n10) dix");
        rt("- [ ] à faire\n- [x] fait");
        rt("> citation\n>");
        rt("```rust\nfn main() { *x* }\n```\napres");
        rt("Voir [le site](https://example.com) ou https://omarchy.org.");
        rt("snake_case et 2 * 3");
        rt("");
        rt("a\n");
        rt("***gras italique***");
        rt("###### h6");
    }

    #[test]
    fn styles() {
        let l = &parse("**a *b* c**")[0];
        assert_eq!(l.spans.len(), 3);
        assert!(l.spans[1].style.bold && l.spans[1].style.italic);
        let l = &parse("***x***")[0];
        assert!(l.spans[0].style.bold && l.spans[0].style.italic);
    }

    #[test]
    fn serialize_model_escapes() {
        let line = Line::plain(Block::Para, "# pas un titre *ni gras*");
        let s = serialize_line(&line);
        assert_eq!(parse(&s)[0], line, "{s}");
        let line = Line::plain(Block::Para, "1. pas une liste");
        let s = serialize_line(&line);
        assert_eq!(parse(&s)[0], line, "{s}");
        let line = Line::plain(Block::Para, "https://x.org en texte brut");
        let s = serialize_line(&line);
        assert_eq!(parse(&s)[0], line, "{s}");
    }

    #[test]
    fn adjacent_styles() {
        let line = Line {
            block: Block::Para,
            spans: vec![
                Span { text: "a".into(), style: Style { bold: true, ..Default::default() } },
                Span { text: "b".into(), style: Style { bold: true, italic: true, ..Default::default() } },
                Span { text: "c".into(), style: Style { italic: true, ..Default::default() } },
            ],
        };
        let s = serialize_line(&line);
        assert_eq!(parse(&s)[0], line, "{s}");
    }

    #[test]
    fn tables() {
        rt("avant\n| a | b |\n|:--|--:|\n| 1 | x \\| y |\n\napres");
        assert_eq!(serialize(&parse("| a |\n|---|\napres")), "| a |\n|---|\n\napres");
        let lines = parse("| a | b |\n|---|---|\n| 1 | 2 |");
        let Block::Table(t) = &lines[0].block else { panic!("{lines:?}") };
        assert_eq!(t.rows, vec![vec!["1".to_string(), "2".to_string()]]);
        let mut t = t.clone();
        t.raw = None;
        t.rows[0][1] = "a|b".into();
        let s = serialize_table(&t);
        assert_eq!(s, "| a   | b    |\n| --- | ---- |\n| 1   | a\\|b |");
        let Block::Table(back) = &parse(&s)[0].block else { panic!() };
        assert_eq!(back.rows[0][1], "a|b");
        // Not a table: no delimiter row.
        assert_eq!(parse("a | b\nc | d")[0].block, Block::Para);
    }

    #[test]
    fn empty_blocks() {
        for s in ["- ", "1. ", "- [ ]", "# ", ">"] {
            let lines = parse(s);
            assert_eq!(serialize(&lines), s, "{lines:?}");
        }
    }
}
