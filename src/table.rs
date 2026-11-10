//! An editable Markdown table, drawn over a placeholder in the text view.
//! Cells wrap their text; column widths share the available width.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::{gdk, glib, prelude::*};

use crate::model::{self, Align, Block, Line, Span, Style, Table};

type Callback = Rc<dyn Fn()>;
type ExitCallback = Rc<dyn Fn(bool)>;

/// Horizontal padding of a cell (CSS padding + border).
const CELL_PAD: i32 = 18;

pub struct TableView {
    data: RefCell<Table>,
    pub root: gtk::Box,
    pub scroller: gtk::ScrolledWindow,
    grid: gtk::Grid,
    controls: gtk::Box,
    wide_button: gtk::Button,
    cells: RefCell<Vec<Vec<gtk::TextView>>>,
    focus: Cell<(usize, usize)>,
    wide: Cell<bool>,
    /// Width available in normal and wide mode.
    avail: Cell<(i32, i32)>,
    /// Last column widths and character width, for the minimap drawing.
    widths: RefCell<Vec<i32>>,
    char_width: Cell<f64>,
    /// Content changed.
    pub on_change: RefCell<Option<Callback>>,
    /// Leave the table: true = after it, false = before it.
    pub on_exit: RefCell<Option<ExitCallback>>,
    pub on_delete: RefCell<Option<Callback>>,
    /// Ctrl+A in a cell whose text is already all selected: select the table.
    pub on_select_all: RefCell<Option<Callback>>,
    /// Width mode toggled, or a column resized.
    pub on_resize: RefCell<Option<Callback>>,
    /// Column widths set by dragging a border (None: automatic).
    manual: RefCell<Vec<Option<i32>>>,
    /// Columns share the width available and wrap (Markdown); otherwise
    /// they keep their text's width and the table scrolls (CSV data).
    fit: Cell<bool>,
    /// Cells selected together: anchor and moving end, (row, column).
    sel: Cell<Option<((usize, usize), (usize, usize))>>,
    /// Cells drawn as selected.
    picked: RefCell<Vec<(usize, usize)>>,
    /// Drag under way: resizing a column (column, width at start), or
    /// started in a cell (selecting cells once it reaches another).
    resizing: Cell<Option<(usize, i32)>>,
    drag_from: Cell<Option<((usize, usize), (f64, f64))>>,
    resize_cursor: Cell<bool>,
    /// Ctrl+Z (false) or Ctrl+Shift+Z / Ctrl+Y (true): the document's
    /// history, which holds the tables too.
    pub on_undo: RefCell<Option<ExitCallback>>,
    colors: Cell<CellColors>,
}

fn small_button(label: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::with_label(crate::i18n::tr(label));
    b.set_tooltip_text(Some(crate::i18n::tr(tip)));
    b.set_can_focus(false);
    b.set_focus_on_click(false);
    b
}

const LINK: &str = "link:";

/// Colours for rendered Markdown in cells: code text, code background,
/// links, search hit, current hit (background, text).
#[derive(Clone, Copy)]
pub struct CellColors {
    pub code_fg: gdk::RGBA,
    pub code_bg: gdk::RGBA,
    pub link: gdk::RGBA,
}

fn ensure_tag(b: &gtk::TextBuffer, name: &str, colors: &CellColors) -> gtk::TextTag {
    let tag = b.tag_table().lookup(name).unwrap_or_else(|| b.create_tag(Some(name), &[]).expect("tag"));
    match name {
        "b" => tag.set_weight(700),
        "i" => tag.set_style(gtk::pango::Style::Italic),
        "s" => tag.set_strikethrough(true),
        "code" => {
            tag.set_foreground_rgba(Some(&colors.code_fg));
            tag.set_background_rgba(Some(&colors.code_bg));
        }
        n if n.starts_with(LINK) => {
            tag.set_foreground_rgba(Some(&colors.link));
            tag.set_underline(gtk::pango::Underline::Single);
        }
        _ => {}
    }
    tag
}

/// Shows a cell's inline Markdown rendered, markers hidden.
fn render_cell(b: &gtk::TextBuffer, md: &str, colors: &CellColors) {
    b.set_text("");
    let mut it = b.start_iter();
    for span in model::parse_inline(md) {
        let mut names: Vec<String> = Vec::new();
        let st = &span.style;
        if st.bold {
            names.push("b".into());
        }
        if st.italic {
            names.push("i".into());
        }
        if st.strike {
            names.push("s".into());
        }
        if st.code {
            names.push("code".into());
        }
        if let Some(u) = &st.link {
            names.push(format!("{LINK}{u}"));
        }
        for n in &names {
            ensure_tag(b, n, colors);
        }
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        b.insert_with_tags_by_name(&mut it, &span.text, &refs);
    }
}

/// `**bold**`, `*italic*`, `~~strike~~` or `` `code` `` just typed before
/// the cursor: the markers go, the text between them takes the style.
fn apply_typed_markup(b: &gtk::TextBuffer, colors: &CellColors) -> bool {
    let cur = b.iter_at_mark(&b.get_insert());
    if cur.tags().iter().any(|t| t.name().as_deref() == Some("code")) {
        return false;
    }
    let before: Vec<char> = b.text(&b.start_iter(), &cur, false).chars().collect();
    let len = before.len();
    for (mk, tag) in [("**", "b"), ("~~", "s"), ("*", "i"), ("`", "code")] {
        let m: Vec<char> = mk.chars().collect();
        let n = m.len();
        if len < 2 * n + 1 || before[len - n..] != m[..] {
            continue;
        }
        let single_star = mk == "*";
        if single_star && before[len - 2] == '*' {
            continue;
        }
        let close = len - n;
        // The nearest opening marker before the closing one.
        let mut open = None;
        let mut i = close;
        while i > n {
            i -= 1;
            if before[i - n..i] == m[..] && !(single_star && (before.get(i).copied() == Some('*') || (i >= 2 && before[i - 2] == '*'))) {
                open = Some(i - n);
                break;
            }
        }
        let Some(o) = open else { continue };
        let inner = &before[o + n..close];
        if inner.is_empty() || inner[0] == ' ' || inner[inner.len() - 1] == ' ' {
            continue;
        }
        let (o, close) = (o as i32, close as i32);
        let (n, len) = (n as i32, len as i32);
        let (mut s, mut e) = (b.iter_at_offset(close), b.iter_at_offset(len));
        b.delete(&mut s, &mut e);
        let (mut s, mut e) = (b.iter_at_offset(o), b.iter_at_offset(o + n));
        b.delete(&mut s, &mut e);
        ensure_tag(b, tag, colors);
        let (s, e) = (b.iter_at_offset(o), b.iter_at_offset(close - n));
        b.apply_tag_by_name(tag, &s, &e);
        return true;
    }
    false
}

fn style_at(it: &gtk::TextIter) -> Style {
    let mut st = Style::default();
    for t in it.tags() {
        match t.name().as_deref() {
            Some("b") => st.bold = true,
            Some("i") => st.italic = true,
            Some("s") => st.strike = true,
            Some("code") => st.code = true,
            Some(n) if n.starts_with(LINK) => st.link = Some(n[LINK.len()..].to_string()),
            _ => {}
        }
    }
    st
}

/// The cell's content as inline Markdown.
fn cell_markdown(b: &gtk::TextBuffer) -> String {
    let mut spans: Vec<Span> = Vec::new();
    let mut it = b.start_iter();
    while !it.is_end() {
        let mut next = it;
        if !next.forward_to_tag_toggle(None::<&gtk::TextTag>) || next.offset() <= it.offset() {
            next = b.end_iter();
        }
        let text = b.text(&it, &next, false).replace('\n', " ");
        let style = style_at(&it);
        match spans.last_mut() {
            Some(l) if l.style == style => l.text.push_str(&text),
            _ => spans.push(Span { text, style }),
        }
        it = next;
    }
    spans.retain(|s| !s.text.is_empty());
    if spans.is_empty() {
        return String::new();
    }
    model::serialize_line(&Line { block: Block::Para, spans })
}

/// Water-filling: columns that want less than an equal share keep their
/// width, the long ones split what is left (never below their minimum).
fn share(want: &[i32], min: &[i32], avail: i32) -> Vec<i32> {
    let n = want.len();
    let mut out = vec![0; n];
    let mut open: Vec<usize> = (0..n).collect();
    let mut left = avail;
    loop {
        if open.is_empty() {
            break;
        }
        let fair = left / open.len() as i32;
        let (fit, rest): (Vec<usize>, Vec<usize>) = open.iter().partition(|&&c| want[c] <= fair);
        if fit.is_empty() {
            for &c in &rest {
                out[c] = fair.max(min[c]);
            }
            break;
        }
        for &c in &fit {
            out[c] = want[c];
            left -= want[c];
        }
        open = rest;
    }
    out
}

impl TableView {
    pub fn new(table: Table) -> Rc<TableView> {
        let grid = gtk::Grid::new();
        grid.add_css_class("mw-grid");
        grid.set_halign(gtk::Align::Start);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&grid)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(true)
            .propagate_natural_height(true)
            .build();
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        controls.add_css_class("mw-table-controls");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
        root.add_css_class("mw-table");
        root.append(&scroller);
        // GTK's own scrollbar showed above the table (drawn in the overlay):
        // this one sits under it, only while the table is wider than its room.
        let hbar = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&scroller.hadjustment()));
        hbar.add_css_class("mw-table-scrollbar");
        hbar.set_visible(false);
        let bar = hbar.clone();
        scroller.hadjustment().connect_changed(move |a| bar.set_visible(a.upper() > a.page_size() + 1.0));
        root.append(&hbar);
        root.append(&controls);
        controls.set_opacity(0.0);
        let wide = table.wide;

        let tv = Rc::new(TableView {
            data: RefCell::new(table),
            root,
            scroller,
            grid,
            controls,
            wide_button: small_button("⇔ Widen", "Let the table extend past the text column"),
            cells: RefCell::new(Vec::new()),
            focus: Cell::new((0, 0)),
            wide: Cell::new(wide),
            avail: Cell::new((600, 900)),
            widths: RefCell::new(Vec::new()),
            char_width: Cell::new(8.0),
            on_change: RefCell::new(None),
            on_exit: RefCell::new(None),
            on_undo: RefCell::new(None),
            on_delete: RefCell::new(None),
            on_select_all: RefCell::new(None),
            on_resize: RefCell::new(None),
            manual: RefCell::new(Vec::new()),
            fit: Cell::new(true),
            sel: Cell::new(None),
            picked: RefCell::new(Vec::new()),
            resizing: Cell::new(None),
            drag_from: Cell::new(None),
            resize_cursor: Cell::new(false),
            colors: Cell::new(CellColors { code_fg: gdk::RGBA::RED, code_bg: gdk::RGBA::WHITE, link: gdk::RGBA::BLUE }),
        });

        let weak = Rc::downgrade(&tv);
        tv.wide_button.connect_clicked(move |_| {
            if let Some(t) = weak.upgrade() {
                t.set_wide(!t.wide.get());
                let cb = t.on_resize.borrow().clone();
                if let Some(cb) = cb {
                    cb();
                }
            }
        });
        tv.controls.append(&tv.wide_button);

        type Act = fn(&Rc<TableView>);
        let actions: [(&str, &str, Act); 5] = [
            ("+ row", "Add a row below the cell (Tab on the last cell)", |t| t.add_row()),
            ("+ column", "Add a column to the right", |t| t.add_col()),
            ("− row", "Delete the row", |t| t.del_row()),
            ("− column", "Delete the column", |t| t.del_col()),
            ("Delete table", "Delete the table", |t| {
                let cb = t.on_delete.borrow().clone();
                if let Some(cb) = cb {
                    cb();
                }
            }),
        ];
        for (label, tip, act) in actions {
            let b = small_button(label, tip);
            let weak = Rc::downgrade(&tv);
            b.connect_clicked(move |_| {
                if let Some(t) = weak.upgrade() {
                    act(&t);
                }
            });
            tv.controls.append(&b);
        }

        // Controls only while editing the table.
        let focus = gtk::EventControllerFocus::new();
        let c = tv.controls.clone();
        focus.connect_enter(move |_| c.set_opacity(1.0));
        let c = tv.controls.clone();
        let weak = Rc::downgrade(&tv);
        focus.connect_leave(move |_| {
            c.set_opacity(0.0);
            // Out of the table: its cells are no longer selected.
            if let Some(t) = weak.upgrade() {
                t.set_cell_selection(None);
            }
        });
        tv.root.add_controller(focus);

        tv.update_wide_button();
        tv.connect_grid_gestures();
        tv.rebuild();
        tv
    }

    pub fn table(&self) -> Table {
        let mut t = self.data.borrow().clone();
        t.wide = self.wide.get();
        t
    }

    pub fn is_wide(&self) -> bool {
        self.wide.get()
    }

    pub fn set_wide(&self, wide: bool) {
        self.wide.set(wide);
        self.update_wide_button();
        self.layout();
    }

    fn update_wide_button(&self) {
        self.wide_button.set_label(crate::i18n::tr(if self.wide.get() { "⇔ Narrow" } else { "⇔ Widen" }));
    }

    /// Widths the table may use: within the text column, and when wide.
    /// Also lays out again when the cells' font changed: a new table gets
    /// the theme's font only after its first layout.
    pub fn set_available(&self, normal: i32, wide: i32) {
        if self.avail.get() != (normal, wide) || self.measure_char_width().is_some_and(|cw| cw != self.char_width.get()) {
            self.avail.set((normal, wide));
            self.layout();
        }
    }

    /// Width of a digit in the cells' current font.
    fn measure_char_width(&self) -> Option<f64> {
        let cells = self.cells.borrow();
        let first = cells.first().and_then(|r| r.first())?;
        let layout = first.create_pango_layout(Some("0000000000"));
        Some((layout.pixel_size().0 as f64 / 10.0).max(4.0))
    }

    pub fn max_width(&self) -> i32 {
        let (n, w) = self.avail.get();
        if self.wide.get() { w } else { n }
    }

    fn rows(&self) -> usize {
        1 + self.data.borrow().rows.len()
    }

    fn cols(&self) -> usize {
        self.data.borrow().cols()
    }

    fn cell(&self, r: usize, c: usize) -> String {
        let d = self.data.borrow();
        if r == 0 { d.header.get(c).cloned() } else { d.rows.get(r - 1).and_then(|row| row.get(c).cloned()) }.unwrap_or_default()
    }

    fn changed(&self) {
        self.data.borrow_mut().raw = None;
        let cb = self.on_change.borrow().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    /// Shares the available width between columns: each wants its longest
    /// text, keeps at least its longest word, and wraps beyond.
    fn layout(&self) {
        let Some(cw) = self.measure_char_width() else { return };
        let cells = self.cells.borrow();
        let px = |chars: usize| (chars as f64 * cw).ceil() as i32 + CELL_PAD;
        let (rows, cols) = (self.rows(), self.cols());
        let mut want = Vec::with_capacity(cols);
        let mut min = Vec::with_capacity(cols);
        for c in 0..cols {
            let texts: Vec<String> = (0..rows).map(|r| self.cell(r, c)).collect();
            let longest = texts.iter().map(|t| t.chars().count()).max().unwrap_or(0).clamp(4, 80);
            let word = texts
                .iter()
                .flat_map(|t| t.split_whitespace().map(|w| w.chars().count()).collect::<Vec<_>>())
                .max()
                .unwrap_or(0)
                .clamp(4, 14);
            want.push(px(longest));
            min.push(px(word));
        }
        let avail = self.max_width() - 2;
        let total: i32 = want.iter().sum();
        let mut manual = self.manual.borrow_mut();
        manual.resize(cols, None);
        let widths: Vec<i32> = if self.fit.get() && manual.iter().all(Option::is_none) {
            if total <= avail { want } else { share(&want, &min, avail) }
        } else {
            // Natural widths (long texts still wrap past 60 characters) and
            // the user's own; the table scrolls sideways when too wide.
            (0..cols).map(|c| manual[c].unwrap_or_else(|| want[c].min(px(60)))).collect()
        };
        drop(manual);
        for row in cells.iter() {
            for (c, cell) in row.iter().enumerate() {
                cell.set_size_request(widths[c], -1);
            }
        }
        self.char_width.set(cw);
        *self.widths.borrow_mut() = widths;
        self.scroller.set_max_content_width(self.max_width().max(120));
    }

    fn rebuild(self: &Rc<Self>) {
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        let mut all = Vec::new();
        let (rows, cols) = (self.rows(), self.cols());
        for r in 0..rows {
            let mut row = Vec::new();
            for c in 0..cols {
                let tv = gtk::TextView::new();
                tv.set_wrap_mode(gtk::WrapMode::WordChar);
                tv.set_accepts_tab(false);
                tv.set_top_margin(5);
                tv.set_bottom_margin(5);
                tv.set_left_margin(8);
                tv.set_right_margin(8);
                render_cell(&tv.buffer(), &self.cell(r, c), &self.colors.get());
                // The cell's own history would undo its initial text.
                tv.buffer().set_enable_undo(false);
                // Typed text takes the style of the character before it.
                let weak = Rc::downgrade(self);
                // Right after a closed span, typing goes on unstyled.
                let plain_next = Rc::new(Cell::new(false));
                tv.buffer().connect_local("insert-text", true, move |args| {
                    let b = args[0].get::<gtk::TextBuffer>().ok()?;
                    let end = args[1].get::<gtk::TextIter>().ok()?;
                    let text = args[2].get::<String>().ok()?;
                    if matches!(text.as_str(), "*" | "~" | "`") {
                        // A closing marker: render what it closes, as in the text.
                        let (b, weak, plain_next) = (b.clone(), weak.clone(), plain_next.clone());
                        glib::idle_add_local_once(move || {
                            if let Some(t) = weak.upgrade() {
                                plain_next.set(apply_typed_markup(&b, &t.colors.get()));
                            }
                        });
                    }
                    let n = text.chars().count() as i32;
                    let start = b.iter_at_offset(end.offset() - n);
                    if plain_next.replace(false) {
                        for tag in start.tags() {
                            b.remove_tag(&tag, &start, &end);
                        }
                    } else if start.offset() > 0 {
                        let mut prev = start;
                        prev.backward_char();
                        for tag in prev.tags() {
                            b.apply_tag(&tag, &start, &end);
                        }
                    }
                    None
                });
                tv.add_css_class("mw-cell");
                if r == 0 {
                    tv.add_css_class("header");
                }
                if c + 1 == cols {
                    tv.add_css_class("last-col");
                }
                if r + 1 == rows {
                    tv.add_css_class("last-row");
                }
                let align = self.data.borrow().align.get(c).copied().unwrap_or(Align::None);
                tv.set_justification(match align {
                    Align::Center => gtk::Justification::Center,
                    Align::Right => gtk::Justification::Right,
                    _ => gtk::Justification::Left,
                });
                let weak = Rc::downgrade(self);
                let view = tv.downgrade();
                tv.buffer().connect_changed(move |b| {
                    let (Some(t), Some(view)) = (weak.upgrade(), view.upgrade()) else { return };
                    let plain = b.text(&b.start_iter(), &b.end_iter(), false);
                    if plain.contains('\n') {
                        // A Markdown cell is one line.
                        let b = b.clone();
                        glib::idle_add_local_once(move || {
                            if let Some(mut it) = b.start_iter().forward_search("\n", gtk::TextSearchFlags::TEXT_ONLY, None).map(|(s, _)| s) {
                                let mut e = it;
                                e.forward_char();
                                b.delete(&mut it, &mut e);
                            }
                        });
                    }
                    let _ = &view;
                    let text = cell_markdown(b);
                    {
                        let mut d = t.data.borrow_mut();
                        let slot = if r == 0 { d.header.get_mut(c) } else { d.rows.get_mut(r - 1).and_then(|row| row.get_mut(c)) };
                        match slot {
                            Some(s) if *s != text => *s = text,
                            _ => return,
                        }
                    }
                    t.layout();
                    t.changed();
                });
                let focus = gtk::EventControllerFocus::new();
                let weak = Rc::downgrade(self);
                focus.connect_enter(move |_| {
                    if let Some(t) = weak.upgrade() {
                        t.focus.set((r, c));
                    }
                });
                tv.add_controller(focus);
                let key = gtk::EventControllerKey::new();
                key.set_propagation_phase(gtk::PropagationPhase::Capture);
                let weak = Rc::downgrade(self);
                let view = tv.downgrade();
                key.connect_key_pressed(move |_, k, _, state| {
                    let (Some(t), Some(view)) = (weak.upgrade(), view.upgrade()) else { return glib::Propagation::Proceed };
                    if t.key(&view, k, state, r, c) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
                });
                tv.add_controller(key);
                self.grid.attach(&tv, c as i32, r as i32, 1, 1);
                row.push(tv);
            }
            all.push(row);
        }
        *self.cells.borrow_mut() = all;
        self.layout();
    }

    /// Column widths, row heights, text lengths per cell and character
    /// width: enough to draw a miniature.
    pub fn geometry(&self) -> (Vec<i32>, Vec<i32>, Vec<Vec<usize>>, f64) {
        let cells = self.cells.borrow();
        let heights = cells.iter().map(|row| row.first().map_or(28, |c| c.height().max(1))).collect();
        let lens = (0..self.rows()).map(|r| (0..self.cols()).map(|c| self.cell(r, c).chars().count()).collect()).collect();
        (self.widths.borrow().clone(), heights, lens, self.char_width.get())
    }

    pub fn set_colors(&self, colors: CellColors) {
        self.colors.set(colors);
        for row in self.cells.borrow().iter() {
            for cell in row {
                let b = cell.buffer();
                let names: Vec<String> = {
                    let mut v = Vec::new();
                    b.tag_table().foreach(|t| {
                        if let Some(n) = t.name() {
                            v.push(n.to_string());
                        }
                    });
                    v
                };
                for n in names.iter().filter(|n| matches!(n.as_str(), "b" | "i" | "s" | "code") || n.starts_with(LINK)) {
                    ensure_tag(&b, n, &colors);
                }
            }
        }
    }

    /// Ctrl+B and friends inside a cell. False when no cell has the focus.
    pub fn toggle_inline(&self, name: &str) -> bool {
        let cells = self.cells.borrow();
        let Some(cell) = cells.iter().flatten().find(|c| c.has_focus()) else { return false };
        let b = cell.buffer();
        let Some((s, e)) = b.selection_bounds() else { return true };
        let tag = ensure_tag(&b, name, &self.colors.get());
        let mut all = true;
        let mut it = s;
        while it.offset() < e.offset() {
            if !it.has_tag(&tag) {
                all = false;
                break;
            }
            it.forward_char();
        }
        if all {
            b.remove_tag(&tag, &s, &e);
        } else {
            b.apply_tag(&tag, &s, &e);
        }
        // Tags do not emit "changed": store the cell by hand.
        let text = cell_markdown(&b);
        let (r, c) = self.focus.get();
        {
            let mut d = self.data.borrow_mut();
            let slot = if r == 0 { d.header.get_mut(c) } else { d.rows.get_mut(r - 1).and_then(|row| row.get_mut(c)) };
            if let Some(slot) = slot {
                *slot = text;
            }
        }
        drop(cells);
        self.changed();
        true
    }

    /// Text of every cell, header row first.
    pub fn cell_texts(&self) -> Vec<Vec<String>> {
        // What is shown (search offsets point into the rendered text).
        self.cells
            .borrow()
            .iter()
            .map(|row| row.iter().map(|c| { let b = c.buffer(); b.text(&b.start_iter(), &b.end_iter(), false).to_string() }).collect())
            .collect()
    }

    /// Marks search hits `(row, col, start, end, current)` in the cells.
    pub fn highlight(&self, hits: &[(usize, usize, i32, i32, bool)], colors: &[gdk::RGBA; 3]) {
        let cells = self.cells.borrow();
        for row in cells.iter() {
            for cell in row {
                let b = cell.buffer();
                let table = b.tag_table();
                for (name, bg, fg) in [("find", Some(colors[0]), None), ("findcur", Some(colors[1]), Some(colors[2]))] {
                    let tag = table.lookup(name).unwrap_or_else(|| b.create_tag(Some(name), &[]).expect("tag"));
                    tag.set_background_rgba(bg.as_ref());
                    tag.set_foreground_rgba(fg.as_ref());
                    b.remove_tag(&tag, &b.start_iter(), &b.end_iter());
                }
            }
        }
        for &(r, c, s, e, current) in hits {
            if let Some(cell) = cells.get(r).and_then(|row| row.get(c)) {
                let b = cell.buffer();
                b.apply_tag_by_name(if current { "findcur" } else { "find" }, &b.iter_at_offset(s), &b.iter_at_offset(e));
            }
        }
    }

    /// Vertical position of a cell inside the table.
    pub fn cell_y(&self, r: usize, c: usize) -> f64 {
        let cells = self.cells.borrow();
        cells
            .get(r)
            .and_then(|row| row.get(c))
            .and_then(|cell| cell.compute_point(&self.root, &gtk::graphene::Point::new(0.0, 0.0)))
            .map_or(0.0, |p| p.y() as f64)
    }

    /// Focus a cell and select a range of its text.
    pub fn select_in_cell(&self, r: usize, c: usize, s: i32, e: i32) {
        let cells = self.cells.borrow();
        if let Some(cell) = cells.get(r).and_then(|row| row.get(c)) {
            cell.grab_focus();
            let b = cell.buffer();
            b.select_range(&b.iter_at_offset(s), &b.iter_at_offset(e));
        }
    }

    /// Focus the first cell of the first (`last` = false) or last row.
    pub fn enter(&self, last: bool) {
        self.focus_cell(if last { self.rows() - 1 } else { 0 }, 0);
    }

    pub fn focus_cell(&self, r: usize, c: usize) {
        let cells = self.cells.borrow();
        let r = r.min(cells.len().saturating_sub(1));
        if let Some(tv) = cells.get(r).and_then(|row| row.get(c.min(row.len().saturating_sub(1)))) {
            tv.grab_focus();
            let b = tv.buffer();
            b.place_cursor(&b.end_iter());
        }
    }

    /// The pointer cursor over every cell (None: the usual text cursor).
    pub fn set_cursor(&self, name: Option<&str>) {
        for cell in self.cells.borrow().iter().flatten() {
            cell.set_cursor_from_name(Some(name.unwrap_or("text")));
        }
    }

    /// Drawn as selected: the table is inside the text view's selection.
    pub fn set_selected(&self, on: bool) {
        if self.root.has_css_class("selected") != on {
            if on {
                self.root.add_css_class("selected");
            } else {
                self.root.remove_css_class("selected");
            }
        }
    }

    /// Unselects the text of every cell.
    pub fn clear_selection(&self) {
        for cell in self.cells.borrow().iter().flatten() {
            let b = cell.buffer();
            if b.has_selection() {
                b.place_cursor(&b.iter_at_mark(&b.get_insert()));
            }
        }
    }

    fn exit(&self, after: bool) {
        let cb = self.on_exit.borrow().clone();
        if let Some(cb) = cb {
            cb(after);
        }
    }

    fn key(self: &Rc<Self>, view: &gtk::TextView, k: gdk::Key, state: gdk::ModifierType, r: usize, c: usize) -> bool {
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let (rows, cols) = (self.rows(), self.cols());
        let back_tab = k == gdk::Key::ISO_Left_Tab || (k == gdk::Key::Tab && shift);
        if let Some(handled) = self.selection_key(view, k, shift, ctrl, r, c) {
            return handled;
        }
        match k {
            gdk::Key::z | gdk::Key::Z | gdk::Key::y | gdk::Key::Y if ctrl => {
                let redo = shift || matches!(k, gdk::Key::y | gdk::Key::Y);
                let cb = self.on_undo.borrow().clone();
                if let Some(cb) = cb {
                    cb(redo);
                }
                true
            }
            gdk::Key::Tab | gdk::Key::ISO_Left_Tab if !back_tab => {
                if c + 1 < cols {
                    self.focus_cell(r, c + 1);
                } else if r + 1 < rows {
                    self.focus_cell(r + 1, 0);
                } else {
                    self.focus.set((r, c));
                    self.add_row();
                    self.focus_cell(r + 1, 0);
                }
                true
            }
            gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
                if c > 0 {
                    self.focus_cell(r, c - 1);
                } else if r > 0 {
                    self.focus_cell(r - 1, cols - 1);
                } else {
                    self.exit(false);
                }
                true
            }
            gdk::Key::Return | gdk::Key::KP_Enter if !ctrl => {
                if r + 1 < rows {
                    self.focus_cell(r + 1, c);
                } else if self.row_empty(r) && r > 1 {
                    // Enter on an empty last row leaves the table.
                    self.focus.set((r, c));
                    self.del_row();
                    self.exit(true);
                } else {
                    self.focus.set((r, c));
                    self.add_row();
                    self.focus_cell(r + 1, c);
                }
                true
            }
            gdk::Key::Return | gdk::Key::KP_Enter => {
                self.exit(true);
                true
            }
            gdk::Key::Up | gdk::Key::Down => {
                // Move between rows only from the first / last wrapped line.
                let b = view.buffer();
                let y = |it: &gtk::TextIter| view.iter_location(it).y();
                let cur = y(&b.iter_at_mark(&b.get_insert()));
                let up = k == gdk::Key::Up;
                let edge = if up { cur == y(&b.start_iter()) } else { cur == y(&b.end_iter()) };
                if !edge {
                    return false;
                }
                match (up, r) {
                    (true, 0) => self.exit(false),
                    (true, _) => self.focus_cell(r - 1, c),
                    (false, _) if r + 1 < rows => self.focus_cell(r + 1, c),
                    (false, _) => self.exit(true),
                }
                true
            }
            gdk::Key::Escape => {
                self.exit(true);
                true
            }
            // Ctrl+A: the cell, then the table (then the text view's own
            // Ctrl+A selects the whole document).
            gdk::Key::a | gdk::Key::A if ctrl && !shift => {
                let b = view.buffer();
                let all = match b.selection_bounds() {
                    Some((s, e)) => s.is_start() && e.is_end(),
                    None => b.char_count() == 0,
                };
                if !all {
                    b.select_range(&b.start_iter(), &b.end_iter());
                    return true;
                }
                let cb = self.on_select_all.borrow().clone();
                if let Some(cb) = cb {
                    cb();
                }
                true
            }
            _ => false,
        }
    }

    fn row_empty(&self, r: usize) -> bool {
        (0..self.cols()).all(|c| self.cell(r, c).trim().is_empty())
    }

    pub fn add_row(self: &Rc<Self>) {
        let (r, c) = self.focus.get();
        {
            let mut d = self.data.borrow_mut();
            let cols = d.cols();
            let at = r.min(d.rows.len());
            d.rows.insert(at, vec![String::new(); cols]);
        }
        self.rebuild();
        self.changed();
        self.focus_cell(r + 1, c);
    }

    pub fn add_col(self: &Rc<Self>) {
        let (r, c) = self.focus.get();
        {
            let mut d = self.data.borrow_mut();
            let at = (c + 1).min(d.cols());
            let n = d.cols() + 1;
            let mut manual = self.manual.borrow_mut();
            if at <= manual.len() {
                manual.insert(at, None);
            }
            d.header.insert(at, crate::i18n::trf("Column {}", &[&n]));
            d.align.insert(at, Align::None);
            for row in d.rows.iter_mut() {
                row.insert(at, String::new());
            }
        }
        self.rebuild();
        self.changed();
        self.focus_cell(r, c + 1);
    }

    pub fn del_row(self: &Rc<Self>) {
        let (r, c) = self.focus.get();
        if r == 0 || self.data.borrow().rows.is_empty() {
            return;
        }
        self.data.borrow_mut().rows.remove(r - 1);
        self.rebuild();
        self.changed();
        self.focus_cell(r.saturating_sub(1).max(if self.rows() > 1 { 1 } else { 0 }), c);
    }

    pub fn del_col(self: &Rc<Self>) {
        let (r, c) = self.focus.get();
        if self.cols() <= 1 {
            return;
        }
        {
            let mut d = self.data.borrow_mut();
            d.header.remove(c);
            d.align.remove(c);
            let mut manual = self.manual.borrow_mut();
            if c < manual.len() {
                manual.remove(c);
            }
            for row in d.rows.iter_mut() {
                if c < row.len() {
                    row.remove(c);
                }
            }
        }
        self.rebuild();
        self.changed();
        self.focus_cell(r, c.saturating_sub(1));
    }

    // -----------------------------------------------------------------------
    // Column widths, sideways scrolling
    // -----------------------------------------------------------------------

    /// CSV data keeps its columns' widths and scrolls; Markdown tables fit.
    pub fn set_fit(&self, fit: bool) {
        if self.fit.replace(fit) != fit {
            self.layout();
        }
    }

    /// Widths set by hand, 0 for automatic (for the session).
    pub fn manual_widths(&self) -> Vec<i32> {
        let m = self.manual.borrow();
        if m.iter().all(Option::is_none) { Vec::new() } else { m.iter().map(|w| w.unwrap_or(0)).collect() }
    }

    pub fn set_manual_widths(&self, widths: &[i32]) {
        *self.manual.borrow_mut() = widths.iter().map(|&w| (w > 0).then_some(w)).collect();
        self.layout();
    }

    fn resized(&self) {
        let cb = self.on_resize.borrow().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    /// Right edges of the columns and bottom edges of the rows, in grid
    /// coordinates.
    fn edges(&self) -> (Vec<f64>, Vec<f64>) {
        let cells = self.cells.borrow();
        let right = |w: &gtk::TextView| w.compute_bounds(&self.grid).map(|b| (b.x() + b.width()) as f64);
        let bottom = |w: &gtk::TextView| w.compute_bounds(&self.grid).map(|b| (b.y() + b.height()) as f64);
        let cols = cells.first().map(|row| row.iter().filter_map(right).collect()).unwrap_or_default();
        let rows = cells.iter().filter_map(|row| row.first().and_then(bottom)).collect();
        (cols, rows)
    }

    fn cell_at(&self, x: f64, y: f64) -> Option<(usize, usize)> {
        let (cols, rows) = self.edges();
        if cols.is_empty() || rows.is_empty() {
            return None;
        }
        let c = cols.partition_point(|&e| e <= x).min(cols.len() - 1);
        let r = rows.partition_point(|&e| e <= y).min(rows.len() - 1);
        Some((r, c))
    }

    /// The column whose right border is under x (a few pixels either side).
    fn border_at(&self, x: f64) -> Option<usize> {
        let (cols, _) = self.edges();
        cols.iter().position(|&e| (x - e).abs() <= 4.0)
    }

    /// An event's position in grid coordinates.
    fn grid_point(&self, ev: &gdk::Event) -> Option<(f64, f64)> {
        let (ex, ey) = ev.position()?;
        let native = self.grid.native()?;
        let (sx, sy) = native.surface_transform();
        let p = native.compute_point(&self.grid, &gtk::graphene::Point::new((ex - sx) as f32, (ey - sy) as f32))?;
        Some((p.x() as f64, p.y() as f64))
    }

    fn connect_grid_gestures(self: &Rc<Self>) {
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |_, x, _| {
            let Some(t) = weak.upgrade() else { return };
            if t.resizing.get().is_some() {
                return;
            }

            let near = t.border_at(x).is_some();
            if t.resize_cursor.replace(near) != near {
                t.set_cursor(near.then_some("col-resize"));
            }
        });
        let weak = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            if let Some(t) = weak.upgrade()
                && t.resize_cursor.replace(false)
            {
                t.set_cursor(None);
            }
        });
        self.grid.add_controller(motion);

        // A press in a cell dragged into another selects cells. The cell's
        // text view takes the press (and the pointer with it), so gestures
        // and motion controllers here see nothing more: raw events do.
        let raw = gtk::EventControllerLegacy::new();
        raw.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        raw.connect_event(move |_, ev| {
            let Some(t) = weak.upgrade() else { return glib::Propagation::Proceed };
            let Some((x, y)) = t.grid_point(ev) else { return glib::Propagation::Proceed };
            match ev.event_type() {
                gdk::EventType::ButtonPress => {
                    let primary = ev.downcast_ref::<gdk::ButtonEvent>().is_some_and(|b| b.button() == gdk::BUTTON_PRIMARY);
                    let plain = !ev.modifier_state().contains(gdk::ModifierType::SHIFT_MASK);
                    if primary && plain && t.border_at(x).is_none() {
                        t.drag_from.set(t.cell_at(x, y).map(|c| (c, (x, y))));
                    }
                }
                gdk::EventType::MotionNotify => {
                    if let Some((start, _)) = t.drag_from.get() {
                        if !ev.modifier_state().contains(gdk::ModifierType::BUTTON1_MASK) {
                            t.drag_from.set(None);
                        } else if let Some(cur) = t.cell_at(x, y)
                            && (cur != start || t.sel.get().is_some())
                        {
                            t.set_cell_selection(Some((start, cur)));
                            // Not a text selection any more.
                            return glib::Propagation::Stop;
                        }
                    }
                }
                gdk::EventType::ButtonRelease => {
                    let selecting = t.drag_from.take().is_some() && t.sel.get().is_some();
                    if selecting {
                        return glib::Propagation::Stop;
                    }
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        self.grid.add_controller(raw);

        let drag = gtk::GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        drag.connect_drag_begin(move |g, x, y| {
            let Some(t) = weak.upgrade() else { return };
            t.drag_from.set(None);
            if let Some(c) = t.border_at(x) {
                g.set_state(gtk::EventSequenceState::Claimed);
                let w = t.widths.borrow().get(c).copied().unwrap_or(80);
                t.resizing.set(Some((c, w)));
                return;
            }
            let Some(cell) = t.cell_at(x, y) else { return };
            let shift = g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
            if shift {
                // Shift+click: from the cell being edited (or the selection's
                // anchor) to this one.
                let anchor = t.sel.get().map_or(t.focus.get(), |(a, _)| a);
                g.set_state(gtk::EventSequenceState::Claimed);
                t.set_cell_selection(Some((anchor, cell)));
                return;
            }
            t.set_cell_selection(None);
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_update(move |g, dx, dy| {
            let Some(t) = weak.upgrade() else { return };
            if let Some((c, w0)) = t.resizing.get() {
                let min = (t.char_width.get() * 3.0) as i32 + CELL_PAD;
                let w = (w0 + dx as i32).max(min);
                {
                    let mut m = t.manual.borrow_mut();
                    m.resize(t.cols(), None);
                    m[c] = Some(w);
                }
                t.layout();
                t.resized();
                return;
            }
            let _ = (g, dy);
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_end(move |_, _, _| {
            let Some(t) = weak.upgrade() else { return };
            if t.resizing.take().is_some() {
                t.resized();
            }
        });
        self.grid.add_controller(drag);
    }

    // -----------------------------------------------------------------------
    // Selecting cells
    // -----------------------------------------------------------------------

    fn sel_rect(&self) -> Option<(usize, usize, usize, usize)> {
        let ((r0, c0), (r1, c1)) = self.sel.get()?;
        Some((r0.min(r1), c0.min(c1), r0.max(r1), c0.max(c1)))
    }

    /// Selects a block of cells (None: none), drawn as selected; the text
    /// selected inside a cell goes.
    pub fn set_cell_selection(&self, sel: Option<((usize, usize), (usize, usize))>) {
        let clamp = |(r, c): (usize, usize)| (r.min(self.rows().saturating_sub(1)), c.min(self.cols().saturating_sub(1)));
        let sel = sel.map(|(a, b)| (clamp(a), clamp(b)));
        self.sel.set(sel);
        let cells = self.cells.borrow();
        let mut picked = self.picked.borrow_mut();
        for (r, c) in picked.drain(..) {
            if let Some(cell) = cells.get(r).and_then(|row| row.get(c)) {
                cell.remove_css_class("picked");
            }
        }
        if let Some((r0, c0, r1, c1)) = self.sel_rect() {
            for r in r0..=r1 {
                for c in c0..=c1 {
                    if let Some(cell) = cells.get(r).and_then(|row| row.get(c)) {
                        cell.add_css_class("picked");
                        let b = cell.buffer();
                        if b.has_selection() {
                            b.place_cursor(&b.iter_at_mark(&b.get_insert()));
                        }
                        picked.push((r, c));
                    }
                }
            }
        }
    }

    /// The selected cells as tab-separated text (rows on lines).
    fn selection_tsv(&self) -> Option<String> {
        let (r0, c0, r1, c1) = self.sel_rect()?;
        let plain = |md: String| model::parse_inline(&md).into_iter().map(|s| s.text).collect::<String>();
        Some((r0..=r1).map(|r| (c0..=c1).map(|c| plain(self.cell(r, c)).replace(['\t', '\n'], " ")).collect::<Vec<_>>().join("\t")).collect::<Vec<_>>().join("\n"))
    }

    fn clear_selected_cells(&self) {
        let Some((r0, c0, r1, c1)) = self.sel_rect() else { return };
        let cells = self.cells.borrow().clone();
        for row in cells.iter().take(r1 + 1).skip(r0) {
            for cell in row.iter().take(c1 + 1).skip(c0) {
                cell.buffer().set_text("");
            }
        }
    }

    /// Text pasted from (r, c): tab-separated values fill the cells to the
    /// right and below, adding rows when needed.
    fn paste_block(self: &Rc<Self>, text: &str, r: usize, c: usize) {
        let lines: Vec<Vec<String>> = text.trim_end_matches(['\n', '\r']).lines().map(|l| l.split('\t').map(literal).collect()).collect();
        {
            let mut d = self.data.borrow_mut();
            let cols = d.cols();
            for (i, values) in lines.iter().enumerate() {
                let row = r + i;
                if row > 0 {
                    while d.rows.len() < row {
                        d.rows.push(vec![String::new(); cols]);
                    }
                }
                for (j, v) in values.iter().enumerate() {
                    let col = c + j;
                    if col >= cols {
                        break;
                    }
                    if row == 0 {
                        d.header[col] = v.clone();
                    } else {
                        d.rows[row - 1][col] = v.clone();
                    }
                }
            }
        }
        self.rebuild();
        self.changed();
        let (h, w) = (lines.len().max(1) - 1, lines.iter().map(Vec::len).max().unwrap_or(1).max(1) - 1);
        self.focus_cell(r, c);
        self.set_cell_selection(Some(((r, c), (r + h, (c + w).min(self.cols() - 1)))));
    }

    /// Keys acting on a cell selection, or starting one (Shift+arrows).
    /// None: not handled here.
    fn selection_key(self: &Rc<Self>, view: &gtk::TextView, k: gdk::Key, shift: bool, ctrl: bool, r: usize, c: usize) -> Option<bool> {
        let active = self.sel.get();
        if ctrl && matches!(k, gdk::Key::v | gdk::Key::V) && !shift {
            let (r, c) = self.sel_rect().map_or((r, c), |(r0, c0, _, _)| (r0, c0));
            let weak = Rc::downgrade(self);
            let view = view.downgrade();
            let clipboard = view.upgrade()?.clipboard();
            glib::spawn_future_local(async move {
                let Ok(Some(text)) = clipboard.read_text_future().await else { return };
                let (Some(t), Some(view)) = (weak.upgrade(), view.upgrade()) else { return };
                if text.contains('\t') || text.trim_end_matches(['\n', '\r']).contains('\n') {
                    t.paste_block(&text, r, c);
                } else {
                    t.set_cell_selection(None);
                    let b = view.buffer();
                    b.begin_user_action();
                    b.delete_selection(true, true);
                    b.insert_at_cursor(text.trim_end_matches(['\n', '\r']));
                    b.end_user_action();
                }
            });
            return Some(true);
        }
        let step = |(r, c): (usize, usize)| -> Option<(usize, usize)> {
            match k {
                gdk::Key::Up if r > 0 => Some((r - 1, c)),
                gdk::Key::Down if r + 1 < self.rows() => Some((r + 1, c)),
                gdk::Key::Left if c > 0 => Some((r, c - 1)),
                gdk::Key::Right if c + 1 < self.cols() => Some((r, c + 1)),
                gdk::Key::Up | gdk::Key::Down | gdk::Key::Left | gdk::Key::Right => Some((r, c)),
                _ => None,
            }
        };
        if shift && !ctrl && matches!(k, gdk::Key::Up | gdk::Key::Down | gdk::Key::Left | gdk::Key::Right) {
            // Left and Right select text inside the cell until its edge.
            let b = view.buffer();
            let cur = b.iter_at_mark(&b.get_insert());
            let at_edge = match k {
                gdk::Key::Left => cur.is_start(),
                gdk::Key::Right => cur.is_end(),
                _ => true,
            };
            if active.is_none() && !at_edge {
                return None;
            }
            let (anchor, end) = active.unwrap_or(((r, c), (r, c)));
            let end = step(end).unwrap_or(end);
            self.set_cell_selection(Some((anchor, end)));
            return Some(true);
        }
        active?;
        match k {
            gdk::Key::c | gdk::Key::C | gdk::Key::x | gdk::Key::X if ctrl => {
                if let Some(tsv) = self.selection_tsv() {
                    view.clipboard().set_text(&tsv);
                }
                if matches!(k, gdk::Key::x | gdk::Key::X) {
                    self.clear_selected_cells();
                }
                Some(true)
            }
            gdk::Key::Delete | gdk::Key::KP_Delete | gdk::Key::BackSpace => {
                self.clear_selected_cells();
                Some(true)
            }
            gdk::Key::Escape => {
                self.set_cell_selection(None);
                Some(true)
            }
            _ => {
                // Moving or typing: back to a single cell. Modifiers alone
                // and shortcuts leave the selection.
                if !ctrl && (k.to_unicode().is_some() || step((r, c)).is_some() || matches!(k, gdk::Key::Tab | gdk::Key::Return)) {
                    self.set_cell_selection(None);
                }
                None
            }
        }
    }
}

/// Inline Markdown that shows `text` literally.
fn literal(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    model::serialize_line(&Line { block: Block::Para, spans: vec![Span { text: text.to_string(), style: Style::default() }] })
}
