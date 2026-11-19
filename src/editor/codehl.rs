//! Syntax highlighting of fenced code blocks in the rich view. The rich
//! buffer has no language of its own: each block (```lang) is highlighted in
//! a scratch GtkSourceView buffer with the theme's scheme, and its colours
//! are copied as `syn:` tags. Only the blocks whose text or language changed
//! are touched again, so the layout of the rest of the text is left alone.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use super::*;

/// Umbrella tag over every highlighted block body.
const SYN: &str = "syn";
const SYN_PREFIX: &str = "syn:";

/// A fenced block: its language and the offsets of its body.
struct CodeBlock {
    lang: sourceview5::Language,
    start: i32,
    end: i32,
}

/// The language named on a fence line (```rust, ~~~ py, ```{.js}).
fn fence_language(fence: &str) -> Option<sourceview5::Language> {
    let rest = fence.trim_start().trim_start_matches(['`', '~']).trim();
    let word: String = rest
        .trim_start_matches(['{', '.'])
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '}' | ',' | '{'))
        .collect::<String>()
        .to_ascii_lowercase();
    if word.is_empty() {
        return None;
    }
    let alias = match word.as_str() {
        "shell" | "bash" | "zsh" | "console" | "shell-session" | "sh" => "sh",
        "py" | "python" | "python3" => "python3",
        "js" | "javascript" | "node" | "mjs" | "cjs" => "js",
        "ts" | "typescript" => "typescript",
        "tsx" => "typescript-jsx",
        "jsx" => "jsx",
        "rs" => "rust",
        "c++" | "cxx" | "hpp" => "cpp",
        "cs" | "csharp" | "c#" => "c-sharp",
        "yml" => "yaml",
        "md" => "markdown",
        "rb" => "ruby",
        "kt" => "kotlin",
        "golang" => "go",
        "dockerfile" | "docker" => "dockerfile",
        "make" | "makefile" => "makefile",
        "ini" | "conf" | "cfg" => "ini",
        "patch" => "diff",
        "html" | "xhtml" | "htm" => "html",
        other => other,
    };
    let lm = sourceview5::LanguageManager::default();
    lm.language(alias)
        .or_else(|| lm.language(&word))
        .or_else(|| lm.guess_language(Some(&format!("file.{word}")), None))
}

impl Tab {
    /// Highlights the code blocks shortly after the text changed (once per
    /// burst of typing).
    pub(super) fn schedule_code_hl(&self) {
        if let Some(id) = self.code_hl_pending.borrow_mut().take() {
            id.remove();
        }
        let me = self.me.borrow().clone();
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            if let Some(t) = me.upgrade() {
                t.code_hl_pending.borrow_mut().take();
                t.highlight_code();
            }
        });
        *self.code_hl_pending.borrow_mut() = Some(id);
    }

    /// The theme changed: every block is coloured again.
    pub(super) fn reset_code_hl(&self) {
        let table = self.buffer.tag_table();
        let mut old = Vec::new();
        table.foreach(|t| {
            if t.name().is_some_and(|n| n.starts_with(SYN_PREFIX)) {
                old.push(t.clone());
            }
        });
        for t in old {
            table.remove(&t);
        }
        if let Some(t) = self.tag(SYN) {
            let (s, e) = self.buffer.bounds();
            self.buffer.remove_tag(&t, &s, &e);
        }
        self.code_hl_done.borrow_mut().clear();
        self.schedule_code_hl();
    }

    fn syn_tags(&self) -> Vec<gtk::TextTag> {
        let mut out = Vec::new();
        self.buffer.tag_table().foreach(|t| {
            if t.name().is_some_and(|n| n.starts_with(SYN_PREFIX)) {
                out.push(t.clone());
            }
        });
        out
    }

    fn clear_syn(&self, syn: &gtk::TextTag, a: i32, b: i32) {
        let s = self.buffer.iter_at_offset(a);
        let e = self.buffer.iter_at_offset(b);
        for t in self.syn_tags() {
            self.buffer.remove_tag(&t, &s, &e);
        }
        self.buffer.remove_tag(syn, &s, &e);
    }

    /// The fenced blocks with a known language.
    fn code_blocks(&self) -> Vec<CodeBlock> {
        let (Some(code), Some(fence)) = (self.tag("codeblock"), self.tag("fence")) else { return Vec::new() };
        let mut out = Vec::new();
        let push = |out: &mut Vec<CodeBlock>, lang: Option<sourceview5::Language>, first: i32, last: i32| {
            if let Some(lang) = lang
                && first <= last
            {
                let start = self.buffer.iter_at_line(first).map_or(0, |i| i.offset());
                let end = self.line_bounds(last).1.offset();
                out.push(CodeBlock { lang, start, end });
            }
        };
        let count = self.buffer.line_count();
        // An opening fence seen: the block's language and first body line.
        let mut open: Option<(Option<sourceview5::Language>, i32)> = None;
        let mut l = 0;
        while l < count {
            let (s, e) = self.line_bounds(l);
            if !s.has_tag(&code) {
                if let Some((lang, first)) = open.take() {
                    push(&mut out, lang, first, l - 1);
                }
                // On to the next code.
                let mut it = s;
                if !it.forward_to_tag_toggle(Some(&code)) {
                    break;
                }
                l = it.line().max(l + 1);
                continue;
            }
            if s.has_tag(&fence) {
                match open.take() {
                    Some((lang, first)) => push(&mut out, lang, first, l - 1),
                    None => open = Some((fence_language(&self.buffer.text(&s, &e, true)), l + 1)),
                }
            }
            l += 1;
        }
        if let Some((lang, first)) = open {
            push(&mut out, lang, first, count - 1);
        }
        out
    }

    pub(super) fn highlight_code(&self) {
        let table = self.buffer.tag_table();
        let syn = match self.tag(SYN) {
            Some(t) => t,
            None => {
                let t = gtk::TextTag::new(Some(SYN));
                table.add(&t);
                t
            }
        };
        let blocks = if self.markdown.get() { self.code_blocks() } else { Vec::new() };
        let mut done = HashSet::new();
        let old = self.code_hl_done.borrow().clone();
        for b in &blocks {
            if b.start >= b.end {
                continue;
            }
            let s = self.buffer.iter_at_offset(b.start);
            let e = self.buffer.iter_at_offset(b.end);
            let text = self.buffer.text(&s, &e, true).to_string();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            b.lang.id().hash(&mut h);
            text.hash(&mut h);
            let key = h.finish();
            let mut probe = s;
            let covered = s.has_tag(&syn) && {
                probe.forward_to_tag_toggle(Some(&syn));
                probe.offset() >= b.end
            };
            if old.contains(&key) && covered {
                done.insert(key);
                continue;
            }
            self.clear_syn(&syn, b.start, b.end);
            self.paint_block(b, &text);
            let s = self.buffer.iter_at_offset(b.start);
            let e = self.buffer.iter_at_offset(b.end);
            self.buffer.apply_tag(&syn, &s, &e);
            done.insert(key);
        }
        // Colours left outside the blocks (a fence deleted, text pasted).
        let mut stale = Vec::new();
        let mut it = self.buffer.start_iter();
        while it.has_tag(&syn) || it.forward_to_tag_toggle(Some(&syn)) {
            let mut end = it;
            end.forward_to_tag_toggle(Some(&syn));
            let (a, z) = (it.offset(), end.offset());
            let mut pos = a;
            for b in blocks.iter().filter(|b| b.end > a && b.start < z) {
                if b.start > pos {
                    stale.push((pos, b.start));
                }
                pos = pos.max(b.end);
            }
            if pos < z {
                stale.push((pos, z));
            }
            if end.is_end() || z <= a {
                break;
            }
            it = end;
        }
        for (a, z) in stale {
            self.clear_syn(&syn, a, z);
        }
        *self.code_hl_done.borrow_mut() = done;
    }

    /// Copies the colours of `text` highlighted as `b.lang` onto the block.
    fn paint_block(&self, b: &CodeBlock, text: &str) {
        let scratch = sourceview5::Buffer::new(None::<&gtk::TextTagTable>);
        scratch.set_style_scheme(self.buffer.style_scheme().as_ref());
        scratch.set_language(Some(&b.lang));
        scratch.set_highlight_syntax(true);
        scratch.set_text(text);
        let (s, e) = scratch.bounds();
        scratch.ensure_highlight(&s, &e);
        let mut it = s;
        while !it.is_end() {
            let mut next = it;
            next.forward_to_tag_toggle(None::<&gtk::TextTag>);
            if next.offset() <= it.offset() {
                break;
            }
            // Tags come lowest priority first: the last one set wins.
            let (mut fg, mut bold, mut italic, mut underline) = (None, false, false, false);
            for t in it.tags() {
                if t.is_foreground_set() {
                    fg = t.foreground_rgba();
                }
                if t.is_weight_set() {
                    bold = t.weight() >= 600;
                }
                if t.is_style_set() {
                    italic = t.style() == pango::Style::Italic;
                }
                if t.is_underline_set() {
                    underline = t.underline() != pango::Underline::None;
                }
            }
            if fg.is_some() || bold || italic || underline {
                let tag = self.syn_tag(fg, bold, italic, underline);
                let a = self.buffer.iter_at_offset(b.start + it.offset());
                let z = self.buffer.iter_at_offset(b.start + next.offset());
                self.buffer.apply_tag(&tag, &a, &z);
            }
            it = next;
        }
    }

    fn syn_tag(&self, fg: Option<gdk::RGBA>, bold: bool, italic: bool, underline: bool) -> gtk::TextTag {
        let fg_name = fg.map(|c| c.to_str().to_string()).unwrap_or_default();
        let name = format!("{SYN_PREFIX}{fg_name}:{}{}{}", bold as u8, italic as u8, underline as u8);
        if let Some(t) = self.tag(&name) {
            return t;
        }
        let t = gtk::TextTag::new(Some(&name));
        if let Some(c) = fg {
            t.set_foreground_rgba(Some(&c));
        }
        if bold {
            t.set_weight(700);
        }
        if italic {
            t.set_style(pango::Style::Italic);
        }
        if underline {
            t.set_underline(pango::Underline::Single);
        }
        let table = self.buffer.tag_table();
        table.add(&t);
        // Search hits stay drawn above the code's colours.
        let top = table.size() - 1;
        for (i, n) in ["findcur", "find"].iter().enumerate() {
            if let Some(f) = self.tag(n) {
                f.set_priority(top - i as i32);
            }
        }
        t
    }
}
