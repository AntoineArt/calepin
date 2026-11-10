//! Foldable regions: Markdown sections, JSON objects and arrays, YAML
//! blocks. Pure functions over text, kept free of GTK so they can be tested.
//!
//! A region keeps its first line visible and hides the following lines up to
//! `end` included.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub start: i32,
    pub end: i32,
}

/// Regions sorted by start line, one per start line (the largest), each
/// hiding at least one line.
fn tidy(mut regions: Vec<Region>) -> Vec<Region> {
    regions.retain(|r| r.end > r.start);
    regions.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    regions.dedup_by_key(|r| r.start);
    regions
}

/// Sections from per-line heading levels: a heading runs until the next
/// heading of the same or a higher level, without the blank lines before it.
pub fn sections(levels: &[Option<u8>], blank: &[bool]) -> Vec<Region> {
    let n = levels.len();
    let mut out = Vec::new();
    for (i, level) in levels.iter().enumerate() {
        let Some(level) = level else { continue };
        let mut end = n - 1;
        for (j, l) in levels.iter().enumerate().skip(i + 1) {
            if l.is_some_and(|l| l <= *level) {
                end = j - 1;
                break;
            }
        }
        while end > i && blank.get(end).copied().unwrap_or(false) {
            end -= 1;
        }
        out.push(Region { start: i as i32, end: end as i32 });
    }
    tidy(out)
}

/// Markdown source: `#` headings outside fenced code.
pub fn markdown(text: &str) -> Vec<Region> {
    let mut levels = Vec::new();
    let mut blank = Vec::new();
    let mut fence: Option<String> = None;
    for line in text.split('\n') {
        let t = line.trim_start();
        let marker: String = t.chars().take_while(|&c| c == '`' || c == '~').collect();
        let level = if let Some(open) = &fence {
            if t.starts_with(open.as_str()) && t[open.len()..].trim().is_empty() {
                fence = None;
            }
            None
        } else if marker.len() >= 3 && marker.chars().all(|c| c == marker.chars().next().unwrap()) {
            fence = Some(marker);
            None
        } else {
            let hashes = line.chars().take_while(|&c| c == '#').count();
            let rest = &line[hashes..];
            ((1..=6).contains(&hashes) && (rest.is_empty() || rest.starts_with(' '))).then_some(hashes as u8)
        };
        levels.push(level);
        blank.push(line.trim().is_empty());
    }
    sections(&levels, &blank)
}

/// JSON (and JSONC): objects and arrays spanning several lines. The closing
/// bracket's line stays visible.
pub fn json(text: &str) -> Vec<Region> {
    let mut out = Vec::new();
    let mut stack: Vec<(char, i32)> = Vec::new();
    let mut line = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => line += 1,
            '"' => {
                while let Some(s) = chars.next() {
                    match s {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        '\n' => {
                            line += 1;
                            break;
                        }
                        _ => {}
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                for s in chars.by_ref() {
                    if s == '\n' {
                        line += 1;
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for s in chars.by_ref() {
                    if s == '\n' {
                        line += 1;
                    }
                    if prev == '*' && s == '/' {
                        break;
                    }
                    prev = s;
                }
            }
            '{' | '[' => stack.push((c, line)),
            '}' | ']' => {
                let open = if c == '}' { '{' } else { '[' };
                if let Some(pos) = stack.iter().rposition(|(o, _)| *o == open) {
                    let (_, start) = stack[pos];
                    stack.truncate(pos);
                    out.push(Region { start, end: line - 1 });
                }
            }
            _ => {}
        }
    }
    tidy(out)
}

/// YAML: a line and the more indented lines under it. A `- ` sequence at
/// the indentation of the key that owns it belongs to that key.
pub fn yaml(text: &str) -> Vec<Region> {
    struct Open {
        line: i32,
        indent: usize,
        key: bool,
    }
    let mut out = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    let mut last = -1;
    for (i, raw) in text.split('\n').enumerate() {
        let t = raw.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let indent = raw.len() - t.len();
        let dash = t == "-" || t.starts_with("- ");
        while let Some(top) = stack.last() {
            let child = top.indent < indent || (top.indent == indent && dash && top.key);
            if child {
                break;
            }
            out.push(Region { start: top.line, end: last });
            stack.pop();
        }
        let content = t.split(" #").next().unwrap_or(t).trim_end();
        stack.push(Open { line: i as i32, indent, key: !dash && content.ends_with(':') });
        last = i as i32;
    }
    for open in stack {
        out.push(Region { start: open.line, end: last });
    }
    tidy(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(start: i32, end: i32) -> Region {
        Region { start, end }
    }

    #[test]
    fn markdown_sections() {
        let text = "# A\ntext\n\n## B\nb\n\n# C\nc\n```\n# not a heading\n```\n";
        assert_eq!(markdown(text), vec![r(0, 4), r(3, 4), r(6, 10)]);
        // A heading with nothing under it hides nothing.
        assert_eq!(markdown("# A\n# B\n"), vec![]);
        assert_eq!(markdown("#hashtag\ntext"), vec![]);
    }

    #[test]
    fn json_brackets() {
        let text = "{\n  \"a\": {\n    \"b\": 1,\n    \"s\": \"{ not [ a bracket\"\n  },\n  \"c\": [1, 2],\n  \"d\": [\n    1\n  ]\n}\n";
        assert_eq!(json(text), vec![r(0, 8), r(1, 3), r(6, 7)]);
        // Comments (JSONC) and an object opened and closed on adjacent lines.
        assert_eq!(json("{ // {\n}\n"), vec![]);
        assert_eq!(json("[\n  /* [ */ 1\n]"), vec![r(0, 1)]);
    }

    #[test]
    fn yaml_blocks() {
        let text = "a:\n  b: 1\n  c:\n    - x\n    - y\n\nd: 2\nitems:\n- one\n- two:\n    z: 1\n# comment\ne: 3\n";
        assert_eq!(yaml(text), vec![r(0, 4), r(2, 4), r(7, 10), r(9, 10)]);
        assert_eq!(yaml("k: v\nl: w\n"), vec![]);
    }
}
