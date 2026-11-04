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
    /// Width mode toggled.
    pub on_resize: RefCell<Option<Callback>>,
    colors: Cell<CellColors>,
}

fn small_button(label: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.set_tooltip_text(Some(tip));
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
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(true)
            .propagate_natural_height(true)
            .build();
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        controls.add_css_class("mw-table-controls");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
        root.add_css_class("mw-table");
        root.append(&scroller);
        root.append(&controls);
        controls.set_opacity(0.0);
        let wide = table.wide;

        let tv = Rc::new(TableView {
            data: RefCell::new(table),
            root,
            scroller,
            grid,
            controls,
            wide_button: small_button("⇔ Élargir", "Laisser le tableau dépasser la colonne de texte"),
            cells: RefCell::new(Vec::new()),
            focus: Cell::new((0, 0)),
            wide: Cell::new(wide),
            avail: Cell::new((600, 900)),
            widths: RefCell::new(Vec::new()),
            char_width: Cell::new(8.0),
            on_change: RefCell::new(None),
            on_exit: RefCell::new(None),
            on_delete: RefCell::new(None),
            on_resize: RefCell::new(None),
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
            ("+ ligne", "Ajouter une ligne sous la cellule (Tab sur la dernière cellule)", |t| t.add_row()),
            ("+ colonne", "Ajouter une colonne à droite", |t| t.add_col()),
            ("− ligne", "Supprimer la ligne", |t| t.del_row()),
            ("− colonne", "Supprimer la colonne", |t| t.del_col()),
            ("Supprimer le tableau", "Supprimer le tableau", |t| {
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
        focus.connect_leave(move |_| c.set_opacity(0.0));
        tv.root.add_controller(focus);

        tv.update_wide_button();
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
        self.wide_button.set_label(if self.wide.get() { "⇔ Réduire" } else { "⇔ Élargir" });
    }

    /// Widths the table may use: within the text column, and when wide.
    pub fn set_available(&self, normal: i32, wide: i32) {
        if self.avail.get() != (normal, wide) {
            self.avail.set((normal, wide));
            self.layout();
        }
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
        let cells = self.cells.borrow();
        let Some(first) = cells.first().and_then(|r| r.first()) else { return };
        let cw = {
            let layout = first.create_pango_layout(Some("0000000000"));
            (layout.pixel_size().0 as f64 / 10.0).max(4.0)
        };
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
        let widths: Vec<i32> = if total <= avail { want } else { share(&want, &min, avail) };
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
                // Typed text takes the style of the character before it.
                tv.buffer().connect_local("insert-text", true, |args| {
                    let b = args[0].get::<gtk::TextBuffer>().ok()?;
                    let end = args[1].get::<gtk::TextIter>().ok()?;
                    let text = args[2].get::<String>().ok()?;
                    let n = text.chars().count() as i32;
                    let start = b.iter_at_offset(end.offset() - n);
                    if start.offset() > 0 {
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
        match k {
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
            d.header.insert(at, format!("Colonne {n}"));
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
}
