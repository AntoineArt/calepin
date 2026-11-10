//! One editor tab: a GtkSourceView with a minimap, and the rich Markdown layer
//! (Notion-style input rules, block handling, Markdown import/export).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::{gdk, gio, glib, pango, prelude::*};
use sourceview5::prelude::*;

use crate::model::{self, Block, Line, Span, Style, Table};
use crate::table::{CellColors, TableView};
use crate::theme::Theme;

/// Longest list prefix (indent, marker, space) given a hanging indent.
const MAX_HANG: i32 = 24;
const INLINE: [&str; 4] = ["b", "i", "s", "code"];
const BLOCKS: [&str; 9] = ["h1", "h2", "h3", "h4", "h5", "h6", "quote", "codeblock", "fence"];
const MARKERS: [char; 3] = ['-', '*', '+'];
const LINK: &str = "link:";
const BULLET: char = '•';
const UNCHECKED: char = '☐';
const CHECKED: char = '☑';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockCmd {
    Para,
    Heading(u8),
    Quote,
    Bullet,
    Numbered,
    Check,
    Code,
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Para,
    Heading(u8),
    Quote,
    Code { fence: bool },
    Bullet { marker: char },
    Numbered { marker: String },
    Check { marker: char, done: bool },
    Table,
}

#[derive(Clone, Debug)]
struct LineInfo {
    start: i32,
    /// Offset of the line end (before the newline).
    end: i32,
    kind: Kind,
    indent: String,
    /// Characters before the content: indent + glyph/number + space.
    prefix: i32,
}

impl LineInfo {
    fn is_list(&self) -> bool {
        matches!(self.kind, Kind::Bullet { .. } | Kind::Numbered { .. } | Kind::Check { .. })
    }
}

pub struct Tab {
    pub id: String,
    pub buffer: sourceview5::Buffer,
    pub view: sourceview5::View,
    /// Minimap, drawn by Calepin: text as bars, tables, search hits.
    pub map: gtk::DrawingArea,
    pub scrolled: gtk::ScrolledWindow,
    pub widget: gtk::Box,
    pub path: RefCell<Option<PathBuf>>,
    pub markdown: Cell<bool>,
    /// Normalised content matching the file on disk (dirty detection).
    pub saved: RefCell<Option<String>>,
    /// Raw text last read from or written to `path`.
    pub disk: RefCell<Option<String>>,
    pub monitor: RefCell<Option<gio::FileMonitor>>,
    busy: Cell<u32>,
    suppress: Cell<bool>,
    pending: Cell<Option<(i32, i32)>>,
    pending_delete: Cell<Option<i32>>,
    overrides: RefCell<Option<Overrides>>,
    on_change: RefCell<Option<Rc<dyn Fn()>>>,
    margin: Cell<i32>,
    /// Embedded tables and their anchors in the buffer.
    tables: RefCell<Vec<TableSlot>>,
    /// Tables are drawn in this overlay, above placeholders in the text: a
    /// widget inside a GtkTextView never receives text from input methods.
    overlay: gtk::Overlay,
    sync_pending: Cell<bool>,
    margin_pending: Cell<bool>,
    sync_passes: Cell<u32>,
    caret_line: Cell<i32>,
    caret_check: Cell<bool>,
    /// Caret offset at the last check, to tell which way it moved.
    caret_offset: Cell<i32>,
    /// Lines whose hanging indent needs a check.
    hang_dirty: Cell<Option<(i32, i32)>>,
    rendering: Cell<bool>,
    /// The Markdown history while the source is shown, back with the rich view.
    rich_history: RefCell<Option<(Vec<(String, i32)>, usize)>>,
    /// The history changed since it was last saved.
    pub hist_dirty: Cell<bool>,
    map_colors: RefCell<[gdk::RGBA; 5]>,
    /// Search hits as y positions in the text, for the minimap.
    hit_marks: RefCell<Vec<(f64, bool)>>,
    find_colors: RefCell<[gdk::RGBA; 3]>,
    cell_colors: Cell<Option<CellColors>>,
    history: RefCell<Vec<(String, i32)>>,
    hist_pos: Cell<usize>,
    hist_timer: RefCell<Option<glib::SourceId>>,
    restoring: Cell<bool>,
    /// Deleted tables, restored when an undo brings their anchor back.
    graveyard: RefCell<Vec<Table>>,
    pub readonly: Cell<bool>,
    /// Set while a CSV file is shown as a table.
    pub csv: RefCell<Option<crate::csv::Sheet>>,
    me: RefCell<std::rc::Weak<Tab>>,
    /// Smooth wheel scrolling: where the animation is heading.
    scroll_target: Cell<Option<f64>>,
    scroll_clock: Cell<i64>,
    zoom_acc: Cell<f64>,
    on_zoom: RefCell<Option<ZoomCallback>>,
}

pub type TabRef = Rc<Tab>;

struct TableSlot {
    anchor: gtk::TextChildAnchor,
    view: Rc<TableView>,
    placeholder: gtk::Box,
    /// Width the overlay gives the table.
    width: Cell<i32>,
}

type ZoomCallback = Rc<dyn Fn(f64)>;
/// A search hit in a table cell: row, column, range, current.
type CellMark = (usize, usize, i32, i32, bool);

/// Styles forced on/off for text typed at a given offset.
type Overrides = (i32, Vec<(String, bool)>);

fn create_tags(buffer: &sourceview5::Buffer) {
    for name in BLOCKS.iter().chain(INLINE.iter()).chain(["done", "lm"].iter()) {
        buffer.create_tag(Some(name), &[]);
    }
    for m in MARKERS {
        buffer.create_tag(Some(&format!("mk:{m}")), &[]);
    }
    // Search hits, above every other style.
    buffer.create_tag(Some("find"), &[]);
    buffer.create_tag(Some("findcur"), &[]);
}

/// A search hit: in the text, or in a table cell.
#[derive(Clone)]
pub struct Hit {
    pub table: Option<(gtk::TextChildAnchor, usize, usize)>,
    pub start: i32,
    pub end: i32,
    /// Document order.
    pub key: (i32, usize, usize, i32),
}

fn opaque(c: gdk::RGBA) -> gdk::RGBA {
    gdk::RGBA::new(c.red(), c.green(), c.blue(), 1.0)
}

impl Tab {
    pub fn new(id: String, theme: &Theme, scheme: Option<&sourceview5::StyleScheme>, _settings: &sourceview5::SearchSettings) -> TabRef {
        let buffer = sourceview5::Buffer::new(None::<&gtk::TextTagTable>);
        buffer.set_highlight_matching_brackets(false);
        buffer.set_max_undo_levels(500);
        if let Some(s) = scheme {
            buffer.set_style_scheme(Some(s));
        }
        create_tags(&buffer);

        let view = sourceview5::View::with_buffer(&buffer);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_top_margin(26);
        view.set_bottom_margin(160);
        view.set_pixels_below_lines(4);
        view.set_pixels_inside_wrap(2);
        view.set_tab_width(4);
        view.set_highlight_current_line(false);
        view.set_hexpand(true);

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&view)
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();

        let map = gtk::DrawingArea::new();
        map.add_css_class("minimap");
        map.set_vexpand(true);
        map.set_content_width(110);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&scrolled));
        overlay.set_hexpand(true);
        // The version, discreetly, under the minimap (it stays in the corner
        // when the minimap is hidden).
        let version = gtk::Label::new(Some(concat!("v", env!("CARGO_PKG_VERSION"))));
        version.add_css_class("version");
        version.set_halign(gtk::Align::End);
        version.set_valign(gtk::Align::End);
        // Without the minimap, the label takes the column's height to stay
        // at the bottom.
        let v = version.clone();
        map.connect_visible_notify(move |m| v.set_vexpand(!m.is_visible()));
        let side = gtk::Box::new(gtk::Orientation::Vertical, 0);
        side.append(&map);
        side.append(&version);
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        widget.append(&overlay);
        widget.append(&side);


        let tab = Rc::new(Tab {
            id,
            buffer,
            view,
            map,
            scrolled,
            widget,
            path: RefCell::new(None),
            markdown: Cell::new(true),
            saved: RefCell::new(None),
            disk: RefCell::new(None),
            monitor: RefCell::new(None),
            busy: Cell::new(0),
            suppress: Cell::new(false),
            pending: Cell::new(None),
            pending_delete: Cell::new(None),
            overrides: RefCell::new(None),
            on_change: RefCell::new(None),
            margin: Cell::new(-1),
            tables: RefCell::new(Vec::new()),
            overlay,
            sync_pending: Cell::new(false),
            margin_pending: Cell::new(false),
            sync_passes: Cell::new(0),
            caret_line: Cell::new(0),
            caret_check: Cell::new(false),
            caret_offset: Cell::new(0),
            hang_dirty: Cell::new(None),
            rendering: Cell::new(false),
            rich_history: RefCell::new(None),
            hist_dirty: Cell::new(false),
            map_colors: RefCell::new([gdk::RGBA::BLACK; 5]),
            hit_marks: RefCell::new(Vec::new()),
            find_colors: RefCell::new([gdk::RGBA::BLACK; 3]),
            cell_colors: Cell::new(None),
            history: RefCell::new(vec![(String::new(), 0)]),
            hist_pos: Cell::new(0),
            hist_timer: RefCell::new(None),
            restoring: Cell::new(false),
            graveyard: RefCell::new(Vec::new()),
            readonly: Cell::new(false),
            csv: RefCell::new(None),
            me: RefCell::new(std::rc::Weak::new()),
            scroll_target: Cell::new(None),
            scroll_clock: Cell::new(0),
            zoom_acc: Cell::new(0.0),
            on_zoom: RefCell::new(None),
        });
        *tab.me.borrow_mut() = Rc::downgrade(&tab);
        tab.apply_theme(theme, scheme);
        tab.connect_signals();
        tab
    }

    pub fn set_on_zoom(&self, f: Rc<dyn Fn(f64)>) {
        *self.on_zoom.borrow_mut() = Some(f);
    }

    fn on_scroll(&self, c: &gtk::EventControllerScroll, dy: f64) -> glib::Propagation {
        let wheel = c.unit() == gdk::ScrollUnit::Wheel;
        if c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
            // Ctrl+wheel zooms, one step per notch (or per ~30 px on a touchpad).
            let acc = self.zoom_acc.get() + if wheel { dy } else { dy / 30.0 };
            let steps = acc.trunc();
            self.zoom_acc.set(acc - steps);
            if steps != 0.0 {
                let cb = self.on_zoom.borrow().clone();
                if let Some(cb) = cb {
                    cb(-steps);
                }
            }
            return glib::Propagation::Stop;
        }
        if !wheel || dy == 0.0 {
            // Touchpads already scroll smoothly, with kinetic deceleration.
            self.scroll_target.set(None);
            return glib::Propagation::Proceed;
        }
        let adj = self.scrolled.vadjustment();
        let step = adj.page_size().powf(2.0 / 3.0);
        let base = self.scroll_target.get().unwrap_or(adj.value());
        let target = (base + dy * step).clamp(adj.lower(), (adj.upper() - adj.page_size()).max(adj.lower()));
        let animating = self.scroll_target.get().is_some();
        self.scroll_target.set(Some(target));
        if !animating {
            self.scroll_clock.set(0);
            let me = self.me.borrow().clone();
            self.view.add_tick_callback(move |_, clock| {
                let Some(t) = me.upgrade() else { return glib::ControlFlow::Break };
                let Some(target) = t.scroll_target.get() else { return glib::ControlFlow::Break };
                let now = clock.frame_time();
                let last = t.scroll_clock.replace(now);
                let dt = if last == 0 { 1.0 / 60.0 } else { ((now - last) as f64 / 1e6).clamp(0.001, 0.1) };
                let adj = t.scrolled.vadjustment();
                let v = adj.value();
                // Ease out: cover ~90 % of the distance in 120 ms.
                let next = v + (target - v) * (1.0 - (-dt / 0.052).exp());
                if (target - next).abs() < 0.5 {
                    adj.set_value(target);
                    t.scroll_target.set(None);
                    return glib::ControlFlow::Break;
                }
                adj.set_value(next);
                glib::ControlFlow::Continue
            });
        }
        glib::Propagation::Stop
    }

    pub fn set_on_change(&self, f: Rc<dyn Fn()>) {
        *self.on_change.borrow_mut() = Some(f);
    }

    fn notify(&self) {
        self.schedule_checkpoint();
        self.map.queue_draw();
        let cb = self.on_change.borrow().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    fn tag(&self, name: &str) -> Option<gtk::TextTag> {
        self.buffer.tag_table().lookup(name)
    }

    pub fn apply_theme(&self, theme: &Theme, scheme: Option<&sourceview5::StyleScheme>) {
        if let Some(s) = scheme {
            self.buffer.set_style_scheme(Some(s));
        }
        let set = |name: &str, f: &dyn Fn(&gtk::TextTag)| {
            if let Some(t) = self.tag(name) {
                f(&t);
            }
        };
        let bright = theme.rgba("bright_foreground");
        let accent = theme.rgba("accent");
        let muted = theme.legible_rgba("muted", 3.0);
        for (name, scale, above, weight) in [
            ("h1", 1.6, 18, 800),
            ("h2", 1.35, 14, 700),
            ("h3", 1.17, 10, 700),
            ("h4", 1.05, 8, 700),
            ("h5", 1.0, 6, 700),
            ("h6", 1.0, 6, 600),
        ] {
            set(name, &|t| {
                t.set_scale(scale);
                t.set_weight(weight);
                t.set_pixels_above_lines(above);
                t.set_pixels_below_lines(6);
                t.set_foreground_rgba(Some(&bright));
            });
        }
        set("b", &|t| t.set_weight(700));
        set("i", &|t| t.set_style(pango::Style::Italic));
        set("s", &|t| t.set_strikethrough(true));
        set("code", &|t| {
            t.set_background_rgba(Some(&opaque(theme.mix("foreground", 0.09))));
            t.set_foreground_rgba(Some(&theme.rgba("red")));
        });
        set("quote", &|t| {
            t.set_style(pango::Style::Italic);
            // Quoted text is content: as readable as body text.
            t.set_foreground_rgba(Some(&theme.legible_rgba("dark_foreground", 4.5)));
            t.set_paragraph_background_rgba(Some(&opaque(theme.mix("accent", 0.07))));
        });
        set("codeblock", &|t| {
            t.set_paragraph_background_rgba(Some(&opaque(theme.mix("foreground", 0.06))));
            t.set_pixels_below_lines(0);
            t.set_pixels_inside_wrap(0);
        });
        set("fence", &|t| {
            t.set_foreground_rgba(Some(&theme.legible_rgba("dark_foreground", 3.0)));
            t.set_scale(0.85);
            // Backticks sit high: keep them off the block's edge.
            t.set_pixels_above_lines(5);
            t.set_pixels_below_lines(3);
        });
        set("lm", &|t| {
            t.set_foreground_rgba(Some(&accent));
            t.set_weight(700);
        });
        set("done", &|t| {
            t.set_strikethrough(true);
            t.set_foreground_rgba(Some(&muted));
        });
        self.buffer.tag_table().foreach(|t| {
            if t.name().is_some_and(|n| n.starts_with(LINK)) {
                t.set_foreground_rgba(Some(&accent));
                t.set_underline(pango::Underline::Single);
            }
        });
        let find = [opaque(theme.mix("accent", 0.28)), theme.rgba("accent"), theme.rgba("background")];
        set("find", &|t| t.set_background_rgba(Some(&find[0])));
        set("findcur", &|t| {
            t.set_background_rgba(Some(&find[1]));
            t.set_foreground_rgba(Some(&find[2]));
        });
        *self.find_colors.borrow_mut() = find;
        let cell = CellColors {
            code_fg: theme.rgba("red"),
            code_bg: opaque(theme.mix("foreground", 0.09)),
            link: theme.rgba("accent"),
        };
        self.cell_colors.set(Some(cell));
        for slot in self.tables.borrow().iter() {
            slot.view.set_colors(cell);
        }
        *self.map_colors.borrow_mut() = [
            theme.rgba("foreground"),
            theme.rgba("muted"),
            theme.rgba("accent"),
            opaque(theme.mix("foreground", 0.10)),
            theme.rgba("background"),
        ];
        self.map.queue_draw();
        let m = self.margin.get();
        self.margin.set(-1);
        self.set_margin(m.max(0));
        // A new theme font applies at the next style update.
        let me = self.me.borrow().clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
            if let Some(t) = me.upgrade() {
                t.refresh_hang();
            }
        });
    }

    /// Horizontal text margin; the Markdown column is centred.
    fn set_margin(&self, m: i32) {
        if self.margin.get() == m {
            return;
        }
        self.margin.set(m);
        self.view.set_left_margin(m);
        self.view.set_right_margin(m);
        if let Some(t) = self.tag("quote") {
            t.set_left_margin(m + 22);
        }
    }

    fn update_margin(&self) {
        let width = self.scrolled.width();
        let m = if self.markdown.get() {
            ((width - 780) / 2).max(34)
        } else {
            12
        };
        self.set_margin(m);
        self.schedule_table_sync();
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.buffer.connect_insert_text(move |_, iter, text| {
            if let Some(t) = weak.upgrade() {
                let l = iter.line();
                t.mark_hang(l, l + text.matches('\n').count() as i32);
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_delete_range(move |_, s, _| {
            if let Some(t) = weak.upgrade() {
                t.mark_hang(s.line(), s.line());
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_insert_text(move |_, iter, text| {
            if let Some(t) = weak.upgrade()
                && t.busy.get() == 0
                && !t.suppress.get()
            {
                // A step at the end of each word, before its space: undo
                // then removes " word" whole, leaving no trailing space.
                if text == " " && t.markdown.get() && !t.restoring.get() {
                    let mut prev = *iter;
                    if prev.backward_char() && prev.char().is_alphanumeric() {
                        t.checkpoint();
                    }
                }
                t.pending.set(Some((iter.offset(), text.chars().count() as i32)));
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_delete_range(move |_, s, e| {
            let Some(t) = weak.upgrade() else { return };
            if !t.tables.borrow().is_empty() {
                let mut it = *s;
                while it.offset() < e.offset() {
                    if let Some(a) = it.child_anchor() {
                        let mut tables = t.tables.borrow_mut();
                        if let Some(i) = tables.iter().position(|x| x.anchor == a) {
                            let slot = tables.remove(i);
                            t.overlay.remove_overlay(&slot.view.root);
                            t.graveyard.borrow_mut().push(slot.view.table());
                        }
                    }
                    if !it.forward_char() {
                        break;
                    }
                }
            }
            if t.busy.get() == 0 {
                t.pending_delete.set(Some(s.offset()));
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_mark_set(move |b, _, mark| {
            if let Some(t) = weak.upgrade()
                && (*mark == b.get_insert() || *mark == b.selection_bound())
            {
                t.update_table_selection();
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_mark_set(move |b, _, mark| {
            let Some(t) = weak.upgrade() else { return };
            if *mark != b.get_insert() || t.caret_check.replace(true) {
                return;
            }
            let w = Rc::downgrade(&t);
            glib::idle_add_local_once(move || {
                if let Some(t) = w.upgrade() {
                    t.caret_check.set(false);
                    t.keep_caret_off_tables();
                    t.keep_caret_off_markers();
                }
            });
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_end_user_action(move |_| {
            if let Some(t) = weak.upgrade() {
                t.after_user_edit();
            }
        });
        let weak = Rc::downgrade(self);
        self.overlay.connect_get_child_position(move |_, child| weak.upgrade().and_then(|t| t.table_position(child)));
        let ov = self.overlay.clone();
        self.scrolled.vadjustment().connect_value_changed(move |_| ov.queue_allocate());
        let weak = Rc::downgrade(self);
        self.buffer.connect_changed(move |_| {
            if let Some(t) = weak.upgrade() {
                t.schedule_table_sync();
                t.update_table_selection();
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_changed(move |_| {
            if let Some(t) = weak.upgrade() {
                t.notify();
            }
        });
        for sig in ["undo", "redo"] {
            let weak = Rc::downgrade(self);
            self.buffer.connect_local(sig, false, move |_| {
                if let Some(t) = weak.upgrade() {
                    t.suppress.set(true);
                    let w = Rc::downgrade(&t);
                    glib::idle_add_local_once(move || {
                        if let Some(t) = w.upgrade() {
                            t.suppress.set(false);
                            if t.markdown.get() {
                                // Undo may join lines of different kinds.
                                let n = t.buffer.line_count();
                                t.normalize_lines(0, n - 1, None);
                                t.restore_tables();
                            }
                        }
                    });
                }
                None
            });
        }

        // Smooth mouse-wheel scrolling and Ctrl+wheel zoom, over the text and
        // the tables drawn in the overlay.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        scroll.connect_scroll(move |c, _, dy| match weak.upgrade() {
            Some(t) => t.on_scroll(c, dy),
            None => glib::Propagation::Proceed,
        });
        self.overlay.add_controller(scroll);

        self.connect_minimap();

        // Centre the text column.
        let weak = Rc::downgrade(self);
        self.scrolled.hadjustment().connect_page_size_notify(move |_| {
            // Notified while GtkTextView allocates itself: changing margins
            // right now would invalidate the layout it is validating (GTK
            // aborts on that). Do it just after.
            if let Some(t) = weak.upgrade()
                && !t.margin_pending.replace(true)
            {
                let w = Rc::downgrade(&t);
                glib::idle_add_local_once(move || {
                    if let Some(t) = w.upgrade() {
                        t.margin_pending.set(false);
                        t.update_margin();
                    }
                });
            }
        });

        // Keys handled before GtkTextView.
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        key.connect_key_pressed(move |_, keyval, _, state| {
            let Some(t) = weak.upgrade() else { return glib::Propagation::Proceed };
            if std::env::var_os("MW_DEBUG").is_some() {
                let f = t.view.root().and_then(|r| r.focus());
                eprintln!("key {keyval:?} focus={:?}", f.map(|w| w.type_().name()));
            }
            if !t.markdown.get() {
                return glib::Propagation::Proceed;
            }
            let mods = state & (gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::SUPER_MASK);
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
            match keyval {
                gdk::Key::z | gdk::Key::Z if ctrl && !shift => {
                    t.undo();
                    return glib::Propagation::Stop;
                }
                gdk::Key::z | gdk::Key::Z | gdk::Key::y | gdk::Key::Y if ctrl => {
                    t.redo();
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
            let handled = match keyval {
                gdk::Key::Return | gdk::Key::KP_Enter if mods.is_empty() => t.handle_enter(),
                gdk::Key::BackSpace if mods.is_empty() => t.handle_backspace(),
                gdk::Key::Delete | gdk::Key::KP_Delete if mods.is_empty() => t.handle_delete(),
                gdk::Key::Tab if mods.is_empty() => t.handle_tab(false),
                gdk::Key::ISO_Left_Tab => t.handle_tab(true),
                gdk::Key::Tab if mods == gdk::ModifierType::SHIFT_MASK => t.handle_tab(true),
                _ => false,
            };
            if handled { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        self.view.add_controller(key);

        // Checkbox toggling and Ctrl+click on links.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.set_button(gdk::BUTTON_PRIMARY);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |g, n, x, y| {
            let Some(t) = weak.upgrade() else { return };
            if n != 1 || !t.markdown.get() {
                return;
            }
            let Some(iter) = t.iter_at_point(x, y) else { return };
            let ctrl = g.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
            if ctrl && let Some(url) = t.link_at(&iter) {
                g.set_state(gtk::EventSequenceState::Claimed);
                launch_uri(&url);
                return;
            }
            if t.is_checkbox(&iter) {
                g.set_state(gtk::EventSequenceState::Claimed);
                t.toggle_checkbox(iter.offset());
            }
        });
        self.view.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |m, x, y| {
            let Some(t) = weak.upgrade() else { return };
            if !t.markdown.get() {
                return;
            }
            let ctrl = m.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
            let pointer = t.iter_at_point(x, y).is_some_and(|it| t.is_checkbox(&it) || (ctrl && t.link_at(&it).is_some()));
            t.view.set_cursor_from_name(Some(if pointer { "pointer" } else { "text" }));
        });
        self.view.add_controller(motion);

        // Clipboard speaks Markdown.
        let weak = Rc::downgrade(self);
        self.view.connect_copy_clipboard(move |v| {
            if let Some(t) = weak.upgrade()
                && t.markdown.get()
            {
                v.stop_signal_emission_by_name("copy-clipboard");
                t.copy_markdown(false);
            }
        });
        let weak = Rc::downgrade(self);
        self.view.connect_cut_clipboard(move |v| {
            if let Some(t) = weak.upgrade()
                && t.markdown.get()
            {
                v.stop_signal_emission_by_name("cut-clipboard");
                t.copy_markdown(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.view.connect_paste_clipboard(move |v| {
            if let Some(t) = weak.upgrade()
                && t.markdown.get()
            {
                v.stop_signal_emission_by_name("paste-clipboard");
                t.paste(false);
            }
        });
    }

    fn iter_at_point(&self, x: f64, y: f64) -> Option<gtk::TextIter> {
        let (bx, by) = self.view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let iter = self.view.iter_at_location(bx, by)?;
        // Only count hits on the glyph itself, not past the line end.
        let rect = self.view.iter_location(&iter);
        if bx < rect.x() || bx > rect.x() + rect.width().max(1) {
            return None;
        }
        Some(iter)
    }

    fn link_at(&self, iter: &gtk::TextIter) -> Option<String> {
        iter.tags().iter().find_map(|t| t.name().and_then(|n| n.strip_prefix(LINK).map(str::to_string)))
    }

    fn is_checkbox(&self, iter: &gtk::TextIter) -> bool {
        matches!(iter.char(), UNCHECKED | CHECKED) && self.tag("lm").is_some_and(|t| iter.has_tag(&t))
    }

    // -----------------------------------------------------------------------
    // Edit pipeline
    // -----------------------------------------------------------------------

    fn begin(&self) {
        self.busy.set(self.busy.get() + 1);
        self.buffer.begin_user_action();
    }

    fn end(&self) {
        self.buffer.end_user_action();
        self.busy.set(self.busy.get() - 1);
    }

    fn after_user_edit(&self) {
        if self.busy.get() > 0 || self.suppress.get() {
            return;
        }
        let pending = self.pending.take();
        let deleted = self.pending_delete.take();
        if !self.markdown.get() {
            return;
        }
        self.begin();
        let touched = pending.map(|(o, _)| o).or(deleted);
        if let Some(off) = touched {
            let line = self.buffer.iter_at_offset(off).line();
            if self.isolate_table_line(line) {
                self.end();
                return;
            }
        }
        if let Some((off, len)) = pending {
            self.inherit(off, len);
            let first = self.buffer.iter_at_offset(off).line();
            let last = self.buffer.iter_at_offset(off + len).line();
            self.normalize_lines(first, last, Some((off, len)));
            if len == 1 {
                let c = self.buffer.iter_at_offset(off).char();
                if matches!(c, '*' | '_' | '~' | '`' | ')' | ' ') {
                    // Undo brings the typed Markdown back ("- ", "**b**")
                    // before the formatting it turned into: a step only
                    // when a rule applies, not at each marker typed.
                    self.busy.set(self.busy.get() - 1);
                    let before = (self.to_markdown(), self.cursor_offset());
                    self.busy.set(self.busy.get() + 1);
                    let len = self.buffer.char_count();
                    let block = c == ' ' && self.block_rules(off);
                    if !block {
                        // Offsets moved after a block rule; inline rules
                        // do not apply to a new block.
                        self.inline_rules(off, c);
                    }
                    if block || self.buffer.char_count() != len {
                        self.push_step(before.0, before.1);
                    }
                }
            }
        } else if let Some(off) = deleted {
            let line = self.buffer.iter_at_offset(off).line();
            self.normalize_lines(line, line, None);
        }
        self.end();
    }

    fn inline_names(iter: &gtk::TextIter) -> Vec<String> {
        iter.tags()
            .iter()
            .filter_map(|t| t.name())
            .filter(|n| INLINE.contains(&n.as_str()) || n.starts_with(LINK))
            .map(|n| n.to_string())
            .collect()
    }

    /// Typed text takes the inline style of its neighbours.
    fn inherit(&self, off: i32, len: i32) {
        let start = self.buffer.iter_at_offset(off);
        let end = self.buffer.iter_at_offset(off + len);
        let next_names = if end.ends_line() { Vec::new() } else { Self::inline_names(&end) };
        let names: Vec<String> = if start.line_offset() > 0 {
            let mut prev = start;
            prev.backward_char();
            Self::inline_names(&prev)
                .into_iter()
                .filter(|n| !n.starts_with(LINK) || next_names.contains(n))
                .collect()
        } else {
            next_names.into_iter().filter(|n| !n.starts_with(LINK)).collect()
        };
        for n in &names {
            self.buffer.apply_tag_by_name(n, &start, &end);
        }
        let mut ov = self.overrides.borrow_mut();
        if let Some((pos, list)) = ov.as_mut() {
            if *pos == off {
                for (name, on) in list.iter() {
                    if *on {
                        self.buffer.apply_tag_by_name(name, &start, &end);
                    } else {
                        self.buffer.remove_tag_by_name(name, &start, &end);
                    }
                }
                *pos = off + len;
            } else {
                *ov = None;
            }
        }
    }

    fn set_override(&self, pos: i32, name: &str, on: bool) {
        let mut ov = self.overrides.borrow_mut();
        match ov.as_mut() {
            Some((p, list)) if *p == pos => {
                list.retain(|(n, _)| n != name);
                list.push((name.to_string(), on));
            }
            _ => *ov = Some((pos, vec![(name.to_string(), on)])),
        }
    }

    fn line_bounds(&self, line: i32) -> (gtk::TextIter, gtk::TextIter) {
        let start = self.buffer.iter_at_line(line).unwrap_or_else(|| self.buffer.end_iter());
        let mut end = start;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        (start, end)
    }

    fn block_names(iter: &gtk::TextIter) -> Vec<String> {
        iter.tags()
            .iter()
            .filter_map(|t| t.name())
            .filter(|n| BLOCKS.contains(&n.as_str()))
            .map(|n| n.to_string())
            .collect()
    }

    /// Makes each line carry one consistent set of block tags, taken from its
    /// first pre-existing character, and keeps checkbox content styling right.
    fn normalize_lines(&self, first: i32, last: i32, exclude: Option<(i32, i32)>) {
        for line in first..=last.min(self.buffer.line_count() - 1) {
            let (start, end) = self.line_bounds(line);
            let mut full_end = end;
            full_end.forward_char();
            let excluded = |o: i32| exclude.is_some_and(|(s, l)| o >= s && o < s + l);
            let mut reference = None;
            let mut it = start;
            while it.offset() < end.offset() {
                if !excluded(it.offset()) {
                    reference = Some(it);
                    break;
                }
                it.forward_char();
            }
            if reference.is_none() && !end.is_end() {
                reference = Some(end);
            }
            let reference = reference.unwrap_or(start);
            let names = Self::block_names(&reference);
            for b in BLOCKS {
                if !names.iter().any(|n| n == b) {
                    self.buffer.remove_tag_by_name(b, &start, &full_end);
                }
            }
            for n in &names {
                self.buffer.apply_tag_by_name(n, &start, &full_end);
            }
            let info = self.line_info(line);
            let content = self.buffer.iter_at_offset(info.start + info.prefix);
            match info.kind {
                Kind::Check { done: true, .. } => self.buffer.apply_tag_by_name("done", &content, &end),
                _ => self.buffer.remove_tag_by_name("done", &start, &full_end),
            }
        }
    }

    /// Lines whose hanging indent may be stale, fixed at the next idle.
    fn mark_hang(&self, first: i32, last: i32) {
        if self.rendering.get() {
            return;
        }
        let range = match self.hang_dirty.get() {
            Some((a, b)) => (a.min(first), b.max(last)),
            None => (first, last),
        };
        if self.hang_dirty.replace(Some(range)).is_some() {
            return;
        }
        let me = self.me.borrow().clone();
        glib::idle_add_local_once(move || {
            if let Some(t) = me.upgrade()
                && let Some((a, b)) = t.hang_dirty.take()
            {
                t.fix_hang(a, b);
            }
        });
    }

    /// Gives lines `first..=last` the hanging indent their prefix calls for,
    /// and the rest of the list too (renumbering 9 to 10 widens the prefix).
    fn fix_hang(&self, first: i32, last: i32) {
        if !self.markdown.get() {
            return;
        }
        let n = self.buffer.line_count();
        let mut line = (first - 1).max(0);
        let mut last = last.min(n - 1);
        while line <= last {
            let info = self.line_info(line);
            let want = (info.is_list() && (1..=MAX_HANG).contains(&info.prefix)).then_some(info.prefix);
            let (start, mut end) = self.line_bounds(line);
            end.forward_char();
            let have = start.tags().iter().find_map(|t| t.name().and_then(|n| n.strip_prefix("hang:").and_then(|k| k.parse::<i32>().ok())));
            if have != want {
                if let Some(k) = have {
                    self.buffer.remove_tag_by_name(&format!("hang:{k}"), &start, &end);
                }
                self.apply_hang(&info, &start, &end);
            }
            if line == last && info.is_list() && last + 1 < n && last < first + 2000 {
                last += 1;
            }
            line += 1;
        }
    }

    /// Wrapped lines of a list item line up with its text, not its bullet.
    fn apply_hang(&self, info: &LineInfo, start: &gtk::TextIter, end: &gtk::TextIter) {
        if info.is_list() && (1..=MAX_HANG).contains(&info.prefix) {
            let tag = self.hang_tag(info.prefix);
            self.buffer.apply_tag(&tag, start, end);
        }
    }

    fn hang_tag(&self, n: i32) -> gtk::TextTag {
        let name = format!("hang:{n}");
        if let Some(t) = self.tag(&name) {
            return t;
        }
        let t = self.buffer.create_tag(Some(&name), &[]).expect("new tag");
        self.set_hang_metrics(&t, n, self.char_width());
        t
    }

    /// A negative indent is Pango's hanging indent: the first line stays at
    /// the margin, the following ones move right by that much.
    fn set_hang_metrics(&self, t: &gtk::TextTag, n: i32, cw: f64) {
        t.set_indent(-((n as f64 * cw).round() as i32));
    }

    /// Width of a character of the text font (the text is monospace).
    fn char_width(&self) -> f64 {
        let layout = self.view.create_pango_layout(Some("0000000000"));
        (layout.pixel_size().0 as f64 / 10.0).max(4.0)
    }

    /// After a zoom or font change.
    pub fn refresh_hang(&self) {
        let cw = self.char_width();
        for n in 1..=MAX_HANG {
            if let Some(t) = self.tag(&format!("hang:{n}")) {
                self.set_hang_metrics(&t, n, cw);
            }
        }
    }

    fn line_info(&self, line: i32) -> LineInfo {
        let (start, end) = self.line_bounds(line);
        let text: Vec<char> = self.buffer.text(&start, &end, true).chars().collect();
        let mut reference = start;
        if start.offset() == end.offset() && !end.is_end() {
            reference = end;
        }
        let names = Self::block_names(&reference);
        let base = LineInfo { start: start.offset(), end: end.offset(), kind: Kind::Para, indent: String::new(), prefix: 0 };
        if self.line_anchor(line).is_some() {
            return LineInfo { kind: Kind::Table, ..base };
        }
        if names.iter().any(|n| n == "codeblock" || n == "fence") {
            return LineInfo { kind: Kind::Code { fence: names.iter().any(|n| n == "fence") }, ..base };
        }
        if let Some(h) = names.iter().find_map(|n| n.strip_prefix('h').and_then(|d| d.parse::<u8>().ok())) {
            return LineInfo { kind: Kind::Heading(h), ..base };
        }
        if names.iter().any(|n| n == "quote") {
            return LineInfo { kind: Kind::Quote, ..base };
        }
        let indent_len = text.iter().take_while(|c| **c == ' ' || **c == '\t').count();
        let indent: String = text[..indent_len].iter().collect();
        let rest = &text[indent_len..];
        let space_after = |i: usize| rest.get(i).is_none_or(|c| *c == ' ');
        let glyph_iter = self.buffer.iter_at_offset(start.offset() + indent_len as i32);
        let marker = MARKERS
            .iter()
            .copied()
            .find(|m| self.tag(&format!("mk:{m}")).is_some_and(|t| glyph_iter.has_tag(&t)))
            .unwrap_or('-');
        let has_lm = self.tag("lm").is_some_and(|t| glyph_iter.has_tag(&t));
        let prefix_of = |glyph_len: usize| (indent_len + glyph_len + usize::from(rest.len() > glyph_len)) as i32;
        match rest.first() {
            Some(&BULLET) if space_after(1) => {
                return LineInfo { kind: Kind::Bullet { marker }, indent, prefix: prefix_of(1), ..base };
            }
            Some(&c @ (UNCHECKED | CHECKED)) if space_after(1) => {
                return LineInfo { kind: Kind::Check { marker, done: c == CHECKED }, indent, prefix: prefix_of(1), ..base };
            }
            _ => {}
        }
        if has_lm {
            let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
            if digits > 0 && matches!(rest.get(digits), Some('.' | ')')) && space_after(digits + 1) {
                let marker: String = rest[..=digits].iter().collect();
                return LineInfo { kind: Kind::Numbered { marker }, indent, prefix: prefix_of(digits + 1), ..base };
            }
        }
        base
    }

    // -----------------------------------------------------------------------
    // Input rules
    // -----------------------------------------------------------------------

    fn text_range(&self, a: i32, b: i32) -> String {
        self.buffer.text(&self.buffer.iter_at_offset(a), &self.buffer.iter_at_offset(b), true).to_string()
    }

    fn insert_glyph(&self, off: i32, glyph: &str, marker: char) -> i32 {
        let mut it = self.buffer.iter_at_offset(off);
        let mk = format!("mk:{marker}");
        self.buffer.insert_with_tags_by_name(&mut it, glyph, &["lm", &mk]);
        self.buffer.insert(&mut it, " ");
        it.offset()
    }

    fn delete_range(&self, a: i32, b: i32) {
        if b > a {
            let mut s = self.buffer.iter_at_offset(a);
            let mut e = self.buffer.iter_at_offset(b);
            self.buffer.delete(&mut s, &mut e);
        }
    }

    fn set_line_block(&self, line: i32, name: Option<&str>) {
        let (start, end) = self.line_bounds(line);
        let mut full = end;
        full.forward_char();
        for b in BLOCKS {
            self.buffer.remove_tag_by_name(b, &start, &full);
        }
        if let Some(n) = name {
            self.buffer.apply_tag_by_name(n, &start, &full);
        }
        // An empty last line has no character to hold the tag: remember it
        // for the next typed character.
        if start.offset() == full.offset() {
            for b in ["h1", "h2", "h3", "h4", "h5", "h6", "quote"] {
                self.set_override(start.offset(), b, Some(b) == name);
            }
        }
    }

    fn block_rules(&self, off: i32) -> bool {
        let cursor = off + 1;
        let iter = self.buffer.iter_at_offset(off);
        let info = self.line_info(iter.line());
        if !matches!(info.kind, Kind::Para | Kind::Bullet { .. } | Kind::Numbered { .. }) {
            return false;
        }
        let before: Vec<char> = self.text_range(info.start, cursor).chars().collect();
        let s: String = before.iter().collect();
        let line = iter.line();
        if info.kind == Kind::Para {
            let hashes = before.iter().take_while(|c| **c == '#').count();
            if (1..=6).contains(&hashes) && before.len() == hashes + 1 {
                self.delete_range(info.start, cursor);
                self.set_line_block(line, Some(&format!("h{hashes}")));
                return true;
            }
            if s == "> " {
                self.delete_range(info.start, cursor);
                self.set_line_block(line, Some("quote"));
                return true;
            }
        }
        let indent_len = before.iter().take_while(|c| **c == ' ' || **c == '\t').count();
        let rest: String = before[indent_len..].iter().collect();
        let glyph_at = info.start + indent_len as i32;
        if info.kind == Kind::Para {
            if let Some(m) = rest.chars().next()
                && MARKERS.contains(&m)
                && rest.chars().count() == 2
            {
                self.delete_range(glyph_at, cursor);
                self.insert_glyph(glyph_at, &BULLET.to_string(), m);
                return true;
            }
            for (pat, glyph) in [("[] ", UNCHECKED), ("[ ] ", UNCHECKED), ("[x] ", CHECKED)] {
                if rest == pat {
                    self.delete_range(glyph_at, cursor);
                    self.insert_glyph(glyph_at, &glyph.to_string(), '-');
                    self.normalize_lines(line, line, None);
                    return true;
                }
            }
            let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
            if (1..=9).contains(&digits) && rest.chars().count() == digits + 2 && matches!(rest.chars().nth(digits), Some('.' | ')')) {
                let s = self.buffer.iter_at_offset(glyph_at);
                let e = self.buffer.iter_at_offset(glyph_at + digits as i32 + 1);
                self.buffer.apply_tag_by_name("lm", &s, &e);
                return true;
            }
        } else {
            // "[] " right after a list marker turns the item into a task.
            let marker = match info.kind {
                Kind::Bullet { marker } => marker,
                _ => '-',
            };
            let after: String = before[info.prefix.min(before.len() as i32) as usize..].iter().collect();
            for (pat, glyph) in [("[] ", UNCHECKED), ("[ ] ", UNCHECKED), ("[x] ", CHECKED)] {
                if after == pat {
                    self.delete_range(glyph_at, cursor);
                    self.insert_glyph(glyph_at, &glyph.to_string(), marker);
                    self.normalize_lines(line, line, None);
                    return true;
                }
            }
        }
        false
    }

    fn has_tag_in(&self, name: &str, a: i32, b: i32) -> bool {
        let Some(tag) = self.tag(name) else { return false };
        let mut it = self.buffer.iter_at_offset(a);
        while it.offset() < b {
            if it.has_tag(&tag) {
                return true;
            }
            if !it.forward_char() {
                break;
            }
        }
        false
    }

    /// Replaces `open content close` (char offsets from `base`) by `content`
    /// carrying `tag`.
    fn wrap_rule(&self, base: i32, k: usize, open: usize, content: usize, close: usize, tag: &str) {
        let k = k as i32;
        let (open, content, close) = (open as i32, content as i32, close as i32);
        self.delete_range(base + k + open + content, base + k + open + content + close);
        self.delete_range(base + k, base + k + open);
        let s = self.buffer.iter_at_offset(base + k);
        let e = self.buffer.iter_at_offset(base + k + content);
        self.buffer.apply_tag_by_name(tag, &s, &e);
        self.set_override(base + k + content, tag, false);
        self.buffer.place_cursor(&e);
    }

    fn inline_rules(&self, off: i32, c: char) {
        let iter = self.buffer.iter_at_offset(off);
        let info = self.line_info(iter.line());
        if matches!(info.kind, Kind::Code { .. } | Kind::Table) {
            return;
        }
        let base = info.start + info.prefix;
        if off < base {
            return;
        }
        let t: Vec<char> = self.text_range(base, off + 1).chars().collect();
        let n = t.len();
        let valid = |a: usize, b: usize| a < b && !t[a].is_whitespace() && !t[b - 1].is_whitespace();
        let code_in = |a: usize, b: usize| self.has_tag_in("code", base + a as i32, base + b as i32);

        match c {
            '`' => {
                if n >= 3 && t[n - 2] != '`'
                    && let Some(k) = (0..n - 2).rev().find(|&k| t[k] == '`')
                    && (k == 0 || t[k - 1] != '`')
                    && k + 1 < n - 1
                    && !code_in(k, n - 1)
                {
                    self.wrap_rule(base, k, 1, n - 2 - k, 1, "code");
                }
            }
            '*' | '_' | '~' => {
                if code_in(n - 1, n) {
                    return;
                }
                let d = c;
                let boundary = |k: usize| d != '_' || k == 0 || !t[k - 1].is_alphanumeric();
                let double = n >= 5 && t[n - 2] == d && (n < 3 || t[n - 3] != d);
                if double {
                    // **content**
                    let mut k = n as isize - 4;
                    while k >= 0 {
                        let ku = k as usize;
                        if t[ku] == d && t[ku + 1] == d && (ku == 0 || t[ku - 1] != d) {
                            if valid(ku + 2, n - 2) && boundary(ku) && !code_in(ku, n) {
                                let tag = if d == '~' { "s" } else { "b" };
                                self.wrap_rule(base, ku, 2, n - 4 - ku, 2, tag);
                            }
                            return;
                        }
                        k -= 1;
                    }
                    return;
                }
                if d == '~' || n < 3 || t[n - 2] == d {
                    return;
                }
                // *content*
                if let Some(k) = (0..n - 2).rev().find(|&k| t[k] == d) {
                    if (k > 0 && t[k - 1] == d) || t[k + 1] == d {
                        return;
                    }
                    if valid(k + 1, n - 1) && boundary(k) && !code_in(k, n) {
                        self.wrap_rule(base, k, 1, n - 2 - k, 1, "i");
                    }
                }
            }
            ')' => {
                // [text](url)
                let Some(p) = (0..n - 1).rev().find(|&k| t[k] == '(') else { return };
                if p == 0 || t[p - 1] != ']' || p + 1 >= n - 1 || t[p + 1..n - 1].iter().any(|c| c.is_whitespace()) {
                    return;
                }
                let Some(o) = (0..p - 1).rev().find(|&k| t[k] == '[') else { return };
                if o + 1 >= p - 1 || (o > 0 && t[o - 1] == '!') || code_in(o, n) {
                    return;
                }
                let url: String = t[p + 1..n - 1].iter().collect();
                let text_len = p - 1 - (o + 1);
                let tag = self.link_tag(&url);
                // Delete "](url)" then "[".
                self.delete_range(base + (p - 1) as i32, base + n as i32);
                self.delete_range(base + o as i32, base + o as i32 + 1);
                let s = self.buffer.iter_at_offset(base + o as i32);
                let e = self.buffer.iter_at_offset(base + (o + text_len) as i32);
                self.buffer.apply_tag(&tag, &s, &e);
                self.set_override(e.offset(), &tag.name().unwrap_or_default(), false);
                self.buffer.place_cursor(&e);
            }
            ' ' => self.autolink(base, &t[..n - 1]),
            _ => {}
        }
    }

    /// Links a bare URL that ends right before `t`'s end.
    fn autolink(&self, base: i32, t: &[char]) {
        let start = t.iter().rposition(|c| c.is_whitespace()).map_or(0, |p| p + 1);
        if let Some(end) = model::match_autolink(t, start)
            && end == t.len()
            && !self.has_tag_in("code", base + start as i32, base + end as i32)
        {
            let url: String = t[start..end].iter().collect();
            if self.link_at(&self.buffer.iter_at_offset(base + start as i32)).is_some() {
                return;
            }
            let tag = self.link_tag(&url);
            let s = self.buffer.iter_at_offset(base + start as i32);
            let e = self.buffer.iter_at_offset(base + end as i32);
            self.buffer.apply_tag(&tag, &s, &e);
            // The typed space must not be part of the link.
            let sp = self.buffer.iter_at_offset(base + end as i32 + 1);
            self.buffer.remove_tag(&tag, &e, &sp);
        }
    }

    fn link_tag(&self, url: &str) -> gtk::TextTag {
        let name = format!("{LINK}{url}");
        if let Some(t) = self.tag(&name) {
            return t;
        }
        let t = self.buffer.create_tag(Some(&name), &[]).expect("new tag");
        // Colours follow the theme through apply_theme; set a default now.
        if let Some(lm) = self.tag("lm")
            && let Some(c) = lm.foreground_rgba()
        {
            t.set_foreground_rgba(Some(&c));
        }
        t.set_underline(pango::Underline::Single);
        t
    }

    // -----------------------------------------------------------------------
    // Keys
    // -----------------------------------------------------------------------

    fn cursor(&self) -> gtk::TextIter {
        self.buffer.iter_at_mark(&self.buffer.get_insert())
    }

    fn scroll_to_cursor(&self) {
        self.view.scroll_mark_onscreen(&self.buffer.get_insert());
    }

    fn handle_enter(&self) -> bool {
        if self.buffer.has_selection() {
            return false;
        }
        self.checkpoint();
        let cur = self.cursor();
        let line = cur.line();
        let info = self.line_info(line);
        let at_end = cur.offset() == info.end;
        match &info.kind {
            Kind::Code { .. } | Kind::Table => false,
            Kind::Para => {
                if at_end && line > 0 && self.table_from_typed_head(line) {
                    return true;
                }
                let text = self.text_range(info.start, info.end);
                let trimmed = text.trim();
                if at_end && (trimmed.starts_with("```") || trimmed.starts_with("~~~")) && trimmed.len() >= 3 {
                    let fence = if trimmed.starts_with('`') { "```" } else { "~~~" };
                    self.begin();
                    self.set_line_block(line, Some("codeblock"));
                    let (s, e) = self.line_bounds(line);
                    self.buffer.apply_tag_by_name("fence", &s, &e);
                    for t in INLINE {
                        self.buffer.remove_tag_by_name(t, &s, &e);
                    }
                    let mut it = self.buffer.iter_at_offset(info.end);
                    self.buffer.insert_with_tags_by_name(&mut it, "\n", &["codeblock", "fence"]);
                    let middle = it.offset();
                    self.buffer.insert_with_tags_by_name(&mut it, "\n", &["codeblock"]);
                    self.buffer.insert_with_tags_by_name(&mut it, fence, &["codeblock", "fence"]);
                    if !it.is_end() {
                        // The old newline now ends the closing fence.
                        let mut nl = it;
                        nl.forward_char();
                        self.buffer.apply_tag_by_name("fence", &it, &nl);
                    }
                    self.buffer.place_cursor(&self.buffer.iter_at_offset(middle));
                    self.end();
                    self.scroll_to_cursor();
                    return true;
                }
                self.begin();
                self.autolink_before_cursor(&info);
                self.end();
                false
            }
            Kind::Heading(_) | Kind::Quote => {
                let empty = info.start == info.end;
                if matches!(info.kind, Kind::Quote) && empty {
                    self.begin();
                    self.set_line_block(line, None);
                    self.end();
                    return true;
                }
                if !at_end {
                    return false;
                }
                self.begin();
                self.autolink_before_cursor(&info);
                let mut it = self.cursor();
                self.buffer.insert(&mut it, "\n");
                self.set_line_block(line + 1, None);
                self.set_line_block(line, Some(&self.block_tag_name(&info.kind)));
                for t in INLINE {
                    self.set_override(it.offset(), t, false);
                }
                self.end();
                self.scroll_to_cursor();
                true
            }
            Kind::Bullet { .. } | Kind::Numbered { .. } | Kind::Check { .. } => {
                let empty = info.end == info.start + info.prefix || info.end == info.start + info.prefix - 1;
                self.begin();
                if empty {
                    let indent_len = info.indent.chars().count() as i32;
                    if indent_len > 0 {
                        self.outdent(line);
                    } else {
                        self.delete_range(info.start + indent_len, info.end);
                    }
                    self.end();
                    return true;
                }
                self.autolink_before_cursor(&info);
                let mut it = self.cursor();
                self.buffer.insert(&mut it, "\n");
                self.buffer.insert(&mut it, &info.indent);
                let at = it.offset();
                match &info.kind {
                    Kind::Bullet { marker } => {
                        self.insert_glyph(at, &BULLET.to_string(), *marker);
                    }
                    Kind::Check { marker, .. } => {
                        self.insert_glyph(at, &UNCHECKED.to_string(), *marker);
                    }
                    Kind::Numbered { marker } => {
                        let digits: String = marker.chars().filter(|c| c.is_ascii_digit()).collect();
                        let delim = marker.chars().last().unwrap_or('.');
                        let next = digits.parse::<u64>().unwrap_or(0) + 1;
                        let mut it = self.buffer.iter_at_offset(at);
                        self.buffer.insert_with_tags_by_name(&mut it, &format!("{next}{delim}"), &["lm"]);
                        self.buffer.insert(&mut it, " ");
                    }
                    _ => {}
                }
                // Text moved to the new line keeps its styles; the prefix is plain.
                let new_line = line + 1;
                let info2 = self.line_info(new_line);
                let s = self.buffer.iter_at_offset(info2.start);
                let e = self.buffer.iter_at_offset(info2.start + info2.prefix);
                for t in INLINE {
                    self.buffer.remove_tag_by_name(t, &s, &e);
                }
                self.normalize_lines(line, new_line, None);
                let pos = self.buffer.iter_at_offset(info2.start + info2.prefix);
                self.buffer.place_cursor(&pos);
                for t in INLINE {
                    self.set_override(pos.offset(), t, false);
                }
                self.end();
                self.scroll_to_cursor();
                true
            }
        }
    }

    fn autolink_before_cursor(&self, info: &LineInfo) {
        let cur = self.cursor().offset();
        let base = info.start + info.prefix;
        if cur > base {
            let t: Vec<char> = self.text_range(base, cur).chars().collect();
            self.autolink(base, &t);
        }
    }

    fn block_tag_name(&self, kind: &Kind) -> String {
        match kind {
            Kind::Heading(n) => format!("h{n}"),
            Kind::Quote => "quote".into(),
            _ => String::new(),
        }
    }

    fn handle_backspace(&self) -> bool {
        if self.buffer.has_selection() {
            return self.delete_selected_table();
        }
        let cur = self.cursor();
        let line = cur.line();
        let info = self.line_info(line);
        let mut prev = cur;
        if prev.backward_char() && prev.child_anchor().is_some() {
            // First Backspace selects the table, the second deletes it.
            self.buffer.select_range(&cur, &prev);
            return true;
        }
        if cur.offset() == info.start && line > 0 && self.line_info(line - 1).kind == Kind::Table {
            if info.start == info.end {
                let (_, e) = self.line_bounds(line - 1);
                self.begin();
                self.delete_range(e.offset(), e.offset() + 1);
                self.end();
            } else {
                let (s, _) = self.line_bounds(line - 1);
                let mut e = s;
                e.forward_char();
                self.buffer.select_range(&s, &e);
            }
            return true;
        }
        match info.kind {
            Kind::Heading(_) | Kind::Quote if cur.offset() == info.start => {
                self.begin();
                self.set_line_block(line, None);
                self.end();
                true
            }
            _ if info.is_list() && cur.offset() == info.start + info.prefix => {
                self.begin();
                let indent_len = info.indent.chars().count() as i32;
                self.delete_range(info.start + indent_len, info.start + info.prefix);
                self.normalize_lines(line, line, None);
                self.end();
                true
            }
            _ => false,
        }
    }

    fn outdent(&self, line: i32) {
        let info = self.line_info(line);
        let chars: Vec<char> = info.indent.chars().collect();
        let n = if chars.first() == Some(&'\t') { 1 } else { chars.iter().take(2).take_while(|c| **c == ' ').count() };
        self.delete_range(info.start, info.start + n as i32);
    }

    fn handle_tab(&self, shift: bool) -> bool {
        let (first, last) = match self.buffer.selection_bounds() {
            Some((s, e)) => (s.line(), e.line()),
            None => {
                let l = self.cursor().line();
                (l, l)
            }
        };
        let lines: Vec<i32> = (first..=last).filter(|&l| self.line_info(l).is_list()).collect();
        if lines.is_empty() {
            return false;
        }
        self.begin();
        for l in lines {
            if shift {
                self.outdent(l);
            } else {
                let mut it = self.buffer.iter_at_line(l).unwrap();
                self.buffer.insert(&mut it, "  ");
            }
        }
        self.end();
        true
    }

    fn toggle_checkbox(&self, off: i32) {
        self.checkpoint();
        let it = self.buffer.iter_at_offset(off);
        let c = it.char();
        let new = if c == CHECKED { UNCHECKED } else { CHECKED };
        let names: Vec<String> = it.tags().iter().filter_map(|t| t.name()).map(|n| n.to_string()).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        self.begin();
        self.delete_range(off, off + 1);
        let mut it = self.buffer.iter_at_offset(off);
        self.buffer.insert_with_tags_by_name(&mut it, &new.to_string(), &refs);
        let line = it.line();
        self.normalize_lines(line, line, None);
        self.end();
    }

    // -----------------------------------------------------------------------
    // Formatting commands (toolbar and shortcuts)
    // -----------------------------------------------------------------------

    fn selected_lines(&self) -> (i32, i32) {
        match self.buffer.selection_bounds() {
            Some((s, mut e)) => {
                if e.line_offset() == 0 && e.line() > s.line() {
                    e.backward_char();
                }
                (s.line(), e.line())
            }
            None => {
                let l = self.cursor().line();
                (l, l)
            }
        }
    }

    pub fn toggle_inline(&self, name: &str) {
        if !self.markdown.get() {
            return;
        }
        if self.tables.borrow().iter().any(|s| s.view.toggle_inline(name)) {
            return;
        }
        self.checkpoint();
        let Some(tag) = self.tag(name) else { return };
        if let Some((s, e)) = self.buffer.selection_bounds() {
            let mut all = true;
            let mut it = s;
            while it.offset() < e.offset() {
                if !it.ends_line() && !it.has_tag(&tag) {
                    all = false;
                    break;
                }
                it.forward_char();
            }
            if all {
                self.buffer.remove_tag(&tag, &s, &e);
            } else {
                self.buffer.apply_tag(&tag, &s, &e);
            }
            self.notify();
        } else {
            let cur = self.cursor();
            let mut active = false;
            if cur.line_offset() > 0 {
                let mut prev = cur;
                prev.backward_char();
                active = prev.has_tag(&tag);
            }
            if let Some((p, list)) = self.overrides.borrow().as_ref()
                && *p == cur.offset()
                && let Some((_, on)) = list.iter().find(|(n, _)| n == name)
            {
                active = *on;
            }
            self.set_override(cur.offset(), name, !active);
        }
    }

    pub fn current_link(&self) -> Option<String> {
        let it = match self.buffer.selection_bounds() {
            Some((s, _)) => s,
            None => self.cursor(),
        };
        self.link_at(&it)
    }

    /// Links the selection to `url`, or removes links when `url` is empty.
    /// Without a selection, inserts the URL as a link.
    pub fn set_link(&self, url: &str) {
        self.checkpoint();
        let url = url.trim();
        self.begin();
        let (s, e) = match self.buffer.selection_bounds() {
            Some(b) => b,
            None if url.is_empty() => {
                let cur = self.cursor();
                // Remove the link around the cursor.
                if let Some(u) = self.link_at(&cur)
                    && let Some(tag) = self.tag(&format!("{LINK}{u}"))
                {
                    let mut s = cur;
                    let mut e = cur;
                    s.backward_to_tag_toggle(Some(&tag));
                    e.forward_to_tag_toggle(Some(&tag));
                    self.buffer.remove_tag(&tag, &s, &e);
                    self.notify();
                }
                self.end();
                return;
            }
            None => {
                let mut it = self.cursor();
                let start = it.offset();
                self.buffer.insert(&mut it, url);
                (self.buffer.iter_at_offset(start), it)
            }
        };
        let names: Vec<String> = {
            let mut v = Vec::new();
            self.buffer.tag_table().foreach(|t| {
                if let Some(n) = t.name()
                    && n.starts_with(LINK)
                {
                    v.push(n.to_string());
                }
            });
            v
        };
        for n in names {
            self.buffer.remove_tag_by_name(&n, &s, &e);
        }
        if !url.is_empty() {
            let tag = self.link_tag(url);
            self.buffer.apply_tag(&tag, &s, &e);
        }
        self.end();
        self.notify();
    }

    fn strip_prefix(&self, line: i32) {
        let info = self.line_info(line);
        if info.is_list() {
            let indent_len = info.indent.chars().count() as i32;
            self.delete_range(info.start + indent_len, info.start + info.prefix);
        }
    }

    fn kind_matches(kind: &Kind, cmd: BlockCmd) -> bool {
        match (kind, cmd) {
            (Kind::Para, BlockCmd::Para) | (Kind::Quote, BlockCmd::Quote) => true,
            (Kind::Heading(a), BlockCmd::Heading(b)) => *a == b,
            (Kind::Bullet { .. }, BlockCmd::Bullet) => true,
            (Kind::Numbered { .. }, BlockCmd::Numbered) => true,
            (Kind::Check { .. }, BlockCmd::Check) => true,
            (Kind::Code { .. }, BlockCmd::Code) => true,
            _ => false,
        }
    }

    pub fn set_block(&self, cmd: BlockCmd) {
        if !self.markdown.get() {
            return;
        }
        self.checkpoint();
        let (first, last) = self.selected_lines();
        let all_same = (first..=last).all(|l| Self::kind_matches(&self.line_info(l).kind, cmd));
        self.begin();
        if cmd == BlockCmd::Code {
            if all_same {
                self.unwrap_code(first);
            } else {
                self.wrap_code(first, last);
            }
            self.end();
            self.notify();
            return;
        }
        let target = if all_same { BlockCmd::Para } else { cmd };
        let mut number = 1;
        for l in first..=last {
            let info = self.line_info(l);
            if matches!(info.kind, Kind::Code { .. } | Kind::Table) {
                continue;
            }
            self.strip_prefix(l);
            let indent_at = info.start + info.indent.chars().count() as i32;
            match target {
                BlockCmd::Para => self.set_line_block(l, None),
                BlockCmd::Heading(n) => self.set_line_block(l, Some(&format!("h{n}"))),
                BlockCmd::Quote => self.set_line_block(l, Some("quote")),
                BlockCmd::Bullet => {
                    self.set_line_block(l, None);
                    self.insert_glyph(indent_at, &BULLET.to_string(), '-');
                }
                BlockCmd::Check => {
                    self.set_line_block(l, None);
                    self.insert_glyph(indent_at, &UNCHECKED.to_string(), '-');
                }
                BlockCmd::Numbered => {
                    self.set_line_block(l, None);
                    let mut it = self.buffer.iter_at_offset(indent_at);
                    self.buffer.insert_with_tags_by_name(&mut it, &format!("{number}."), &["lm"]);
                    self.buffer.insert(&mut it, " ");
                    number += 1;
                }
                BlockCmd::Code => {}
            }
            self.normalize_lines(l, l, None);
        }
        self.end();
        self.notify();
    }

    fn wrap_code(&self, first: i32, last: i32) {
        for l in first..=last {
            self.strip_prefix(l);
            let (s, e) = self.line_bounds(l);
            for t in INLINE.iter().chain(["done", "lm"].iter()) {
                self.buffer.remove_tag_by_name(t, &s, &e);
            }
            self.set_line_block(l, Some("codeblock"));
        }
        // Closing fence first so `first` stays valid.
        let (_, e) = self.line_bounds(last);
        let end_off = e.offset();
        let mut it = e;
        self.buffer.insert_with_tags_by_name(&mut it, "\n```", &["codeblock", "fence"]);
        let nl = self.buffer.iter_at_offset(end_off);
        let mut nl2 = nl;
        nl2.forward_char();
        self.buffer.remove_tag_by_name("fence", &nl, &nl2);
        let mut it = self.buffer.iter_at_line(first).unwrap();
        self.buffer.insert_with_tags_by_name(&mut it, "```\n", &["codeblock", "fence"]);
    }

    fn unwrap_code(&self, line: i32) {
        let is_code = |l: i32| matches!(self.line_info(l).kind, Kind::Code { .. });
        let mut first = line;
        while first > 0 && is_code(first - 1) {
            first -= 1;
        }
        let mut last = line;
        while last + 1 < self.buffer.line_count() && is_code(last + 1) {
            last += 1;
        }
        let fence = |l: i32| matches!(self.line_info(l).kind, Kind::Code { fence: true });
        for l in first..=last {
            self.set_line_block(l, None);
        }
        if fence(last) || matches!(self.line_info(last).kind, Kind::Para) && self.text_range(self.line_info(last).start, self.line_info(last).end).trim_start().starts_with("```") {
            let (mut s, e) = self.line_bounds(last);
            // Take the preceding newline with the closing fence.
            s.backward_char();
            self.delete_range(s.offset(), e.offset());
        }
        let first_text = {
            let i = self.line_info(first);
            self.text_range(i.start, i.end)
        };
        if first_text.trim_start().starts_with("```") || first_text.trim_start().starts_with("~~~") {
            let (s, e) = self.line_bounds(first);
            let mut e = e;
            e.forward_char();
            self.delete_range(s.offset(), e.offset());
        }
    }

    pub fn clear_format(&self) {
        if !self.markdown.get() {
            return;
        }
        self.checkpoint();
        let (first, last) = self.selected_lines();
        self.begin();
        let (s, e) = match self.buffer.selection_bounds() {
            Some(b) => b,
            None => {
                let (s, e) = self.line_bounds(first);
                (s, e)
            }
        };
        let mut names: Vec<String> = INLINE.iter().map(|s| s.to_string()).collect();
        self.buffer.tag_table().foreach(|t| {
            if let Some(n) = t.name()
                && n.starts_with(LINK)
            {
                names.push(n.to_string());
            }
        });
        for n in &names {
            self.buffer.remove_tag_by_name(n, &s, &e);
        }
        let cur = self.cursor().offset();
        for n in &names {
            self.set_override(cur, n, false);
        }
        for l in first..=last {
            if matches!(self.line_info(l).kind, Kind::Code { .. } | Kind::Table) {
                continue;
            }
            self.strip_prefix(l);
            self.set_line_block(l, None);
            self.normalize_lines(l, l, None);
        }
        self.end();
        self.notify();
    }

    // -----------------------------------------------------------------------
    // Model conversion
    // -----------------------------------------------------------------------

    fn style_at(&self, it: &gtk::TextIter) -> Style {
        let mut st = Style::default();
        for t in it.tags() {
            let Some(n) = t.name() else { continue };
            match n.as_str() {
                "b" => st.bold = true,
                "i" => st.italic = true,
                "s" => st.strike = true,
                "code" => st.code = true,
                n if n.starts_with(LINK) => st.link = Some(n[LINK.len()..].to_string()),
                _ => {}
            }
        }
        st
    }

    fn spans(&self, a: i32, b: i32) -> Vec<Span> {
        let mut out: Vec<Span> = Vec::new();
        let mut it = self.buffer.iter_at_offset(a);
        while it.offset() < b {
            let mut next = it;
            next.forward_to_tag_toggle(None::<&gtk::TextTag>);
            if next.offset() > b || next.offset() <= it.offset() {
                next = self.buffer.iter_at_offset(b);
            }
            let text = self.buffer.text(&it, &next, true).to_string();
            let style = self.style_at(&it);
            match out.last_mut() {
                Some(last) if last.style == style => last.text.push_str(&text),
                _ => out.push(Span { text, style }),
            }
            it = next;
        }
        out.retain(|s| !s.text.is_empty());
        out
    }

    fn line_model(&self, line: i32, from: Option<i32>, to: Option<i32>) -> Line {
        let info = self.line_info(line);
        let end = to.unwrap_or(info.end).min(info.end);
        let content_start = (info.start + info.prefix).max(from.unwrap_or(0));
        let block = match &info.kind {
            Kind::Table => {
                let table = self
                    .line_anchor(line)
                    .and_then(|(_, a)| self.tables.borrow().iter().find(|x| x.anchor == a).map(|x| x.view.table()));
                return match table {
                    Some(t) => Line { block: Block::Table(t), spans: Vec::new() },
                    None => Line::plain(Block::Para, ""),
                };
            }
            Kind::Code { fence } => {
                let text = self.text_range(from.unwrap_or(info.start).max(info.start), end);
                return Line::plain(if *fence { Block::Fence } else { Block::Code }, &text);
            }
            Kind::Para => Block::Para,
            Kind::Heading(n) => Block::Heading(*n),
            Kind::Quote => Block::Quote,
            Kind::Bullet { marker } => Block::Bullet { indent: info.indent.clone(), marker: *marker },
            Kind::Numbered { marker } => Block::Numbered { indent: info.indent.clone(), marker: marker.clone() },
            Kind::Check { marker, done } => Block::Check { indent: info.indent.clone(), marker: *marker, done: *done },
        };
        let start = if matches!(info.kind, Kind::Para | Kind::Heading(_) | Kind::Quote) {
            from.unwrap_or(info.start).max(info.start)
        } else {
            content_start
        };
        let mut spans = self.spans(start, end);
        for sp in spans.iter_mut() {
            if sp.text.contains('\u{fffc}') {
                sp.text = sp.text.replace('\u{fffc}', "");
            }
        }
        spans.retain(|s| !s.text.is_empty());
        Line { block, spans }
    }

    fn model_range(&self, a: i32, b: i32) -> Vec<Line> {
        let first = self.buffer.iter_at_offset(a).line();
        let last = self.buffer.iter_at_offset(b).line();
        let mut lines: Vec<Line> = (first..=last)
            .map(|l| {
                let from = (l == first).then_some(a);
                let to = (l == last).then_some(b);
                self.line_model(l, from, to)
            })
            .collect();
        fix_fences(&mut lines);
        lines
    }

    pub fn to_markdown(&self) -> String {
        let (s, e) = self.buffer.bounds();
        model::serialize(&self.model_range(s.offset(), e.offset()))
    }

    pub fn content(&self) -> String {
        if let Some(sheet) = self.csv.borrow().as_ref() {
            let (s, e) = self.buffer.bounds();
            let rows = crate::csv::rows_of(&self.model_range(s.offset(), e.offset()));
            return sheet.text_for(&rows);
        }
        if self.markdown.get() {
            self.to_markdown()
        } else {
            let (s, e) = self.buffer.bounds();
            self.buffer.text(&s, &e, true).to_string()
        }
    }

    fn render_line(&self, it: &mut gtk::TextIter, line: &Line) {
        let block_tag: Option<String> = match &line.block {
            Block::Heading(n) => Some(format!("h{}", n.clamp(&1, &6))),
            Block::Quote => Some("quote".into()),
            _ => None,
        };
        let put_spans = |it: &mut gtk::TextIter, spans: &[Span], extra: &[&str]| {
            for span in spans {
                let mut names: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
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
                    names.push(self.link_tag(u).name().unwrap_or_default().to_string());
                }
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                self.buffer.insert_with_tags_by_name(it, &span.text, &refs);
            }
        };
        match &line.block {
            Block::Table(t) => {
                self.insert_table_at(it, t.clone());
            }
            Block::Code => self.buffer.insert_with_tags_by_name(it, &line.text(), &["codeblock"]),
            Block::Fence => self.buffer.insert_with_tags_by_name(it, &line.text(), &["codeblock", "fence"]),
            Block::Para | Block::Heading(_) | Block::Quote => {
                let extra: Vec<&str> = block_tag.as_deref().into_iter().collect();
                put_spans(it, &line.spans, &extra);
            }
            Block::Bullet { indent, marker } => {
                self.buffer.insert(it, indent);
                self.buffer.insert_with_tags_by_name(it, &BULLET.to_string(), &["lm", &format!("mk:{marker}")]);
                self.buffer.insert(it, " ");
                put_spans(it, &line.spans, &[]);
            }
            Block::Check { indent, marker, done } => {
                self.buffer.insert(it, indent);
                let g = if *done { CHECKED } else { UNCHECKED };
                self.buffer.insert_with_tags_by_name(it, &g.to_string(), &["lm", &format!("mk:{marker}")]);
                self.buffer.insert(it, " ");
                put_spans(it, &line.spans, if *done { &["done"] } else { &[] });
            }
            Block::Numbered { indent, marker } => {
                self.buffer.insert(it, indent);
                self.buffer.insert_with_tags_by_name(it, marker, &["lm"]);
                self.buffer.insert(it, " ");
                put_spans(it, &line.spans, &[]);
            }
        }
    }

    fn line_block_tags(line: &Line) -> Vec<String> {
        match &line.block {
            Block::Heading(n) => vec![format!("h{}", n.clamp(&1, &6))],
            Block::Quote => vec!["quote".into()],
            Block::Code => vec!["codeblock".into()],
            Block::Fence => vec!["codeblock".into(), "fence".into()],
            _ => vec![],
        }
    }

    fn insert_lines(&self, it: &mut gtk::TextIter, lines: &[Line]) {
        for (i, line) in lines.iter().enumerate() {
            self.render_line(it, line);
            if i + 1 < lines.len() {
                let tags = Self::line_block_tags(line);
                let refs: Vec<&str> = tags.iter().map(String::as_str).collect();
                self.buffer.insert_with_tags_by_name(it, "\n", &refs);
            }
        }
    }

    /// Replaces the whole buffer (not undoable).
    /// A CSV file asked in rich mode shows as a table, or as plain text when
    /// a table cannot hold it.
    pub fn set_content(&self, text: &str, markdown: bool) {
        *self.csv.borrow_mut() = None;
        *self.rich_history.borrow_mut() = None;
        let kind = self.path.borrow().as_deref().and_then(crate::csv::kind);
        match kind {
            Some(delim) if markdown => {
                let delim = delim.unwrap_or_else(|| crate::csv::guess_delimiter(text));
                match crate::csv::to_markdown(text, delim) {
                    Some(md) => {
                        self.render_content(&md, true);
                        let (s, e) = self.buffer.bounds();
                        let baseline = crate::csv::rows_of(&self.model_range(s.offset(), e.offset()));
                        // The file's conventions (line ends, final newline)
                        // win over those of a raw edit.
                        let file = self.disk.borrow().clone();
                        let format = crate::csv::format_of(file.as_deref().unwrap_or(text), delim);
                        let disk = file.filter(|d| d != text).and_then(|d| crate::csv::shown_rows(&d, delim).map(|r| (r, d)));
                        *self.csv.borrow_mut() = Some(crate::csv::Sheet { format, baseline, text: text.to_string(), disk });
                    }
                    None => self.render_content(text, false),
                }
            }
            _ => self.render_content(text, markdown),
        }
        self.reset_history();
    }

    /// Rich formatting applies: Markdown, not a CSV table.
    pub fn formatting(&self) -> bool {
        self.markdown.get() && self.csv.borrow().is_none()
    }

    /// After the path changed: a table saved as CSV becomes a CSV sheet, a
    /// sheet saved under another name becomes Markdown again.
    pub fn retarget(&self, path: &std::path::Path) {
        if !self.markdown.get() {
            return;
        }
        let kind = crate::csv::kind(path);
        let mut csv = self.csv.borrow_mut();
        let Some(delim) = kind else {
            *csv = None;
            return;
        };
        match csv.as_mut() {
            // Same delimiter: an untouched sheet still writes its original text.
            Some(s) if delim.is_none_or(|d| d == s.format.delim) => {}
            Some(s) => {
                s.format.delim = delim.unwrap_or(',');
                s.baseline.clear();
                s.text.clear();
                s.disk = None;
            }
            None => {
                let format = crate::csv::Format { delim: delim.unwrap_or(','), crlf: false, final_newline: true };
                *csv = Some(crate::csv::Sheet { format, baseline: Vec::new(), text: String::new(), disk: None });
            }
        }
    }

    /// Replaces the buffer without touching the undo history.
    fn render_content(&self, text: &str, markdown: bool) {
        // GTK strings cannot hold NUL.
        let owned;
        let text = if text.contains('\0') {
            owned = text.replace('\0', "␀");
            owned.as_str()
        } else {
            text
        };
        self.busy.set(self.busy.get() + 1);
        self.rendering.set(true);
        self.buffer.begin_irreversible_action();
        self.markdown.set(markdown);
        let (mut s, mut e) = self.buffer.bounds();
        self.buffer.delete(&mut s, &mut e);
        for slot in self.tables.borrow_mut().drain(..) {
            self.overlay.remove_overlay(&slot.view.root);
        }
        self.graveyard.borrow_mut().clear();
        *self.overrides.borrow_mut() = None;
        if markdown {
            self.buffer.set_language(None);
            let mut it = self.buffer.start_iter();
            self.insert_lines(&mut it, &model::parse(text));
            for line in 0..self.buffer.line_count() {
                let info = self.line_info(line);
                if info.is_list() {
                    let (start, mut end) = self.line_bounds(line);
                    end.forward_char();
                    self.apply_hang(&info, &start, &end);
                }
            }
        } else {
            self.buffer.set_text(text);
            self.buffer.set_language(self.guess_language().as_ref());
        }
        self.buffer.end_irreversible_action();
        self.rendering.set(false);
        // Markdown tabs undo through Markdown snapshots (see `undo`): GTK's
        // own history knows the text, not its formatting.
        self.buffer.set_enable_undo(!markdown);
        self.busy.set(self.busy.get() - 1);
        // Line numbers for code, not for prose or plain text.
        self.view.set_show_line_numbers(!markdown && self.buffer.language().is_some_and(|l| l.id() != "markdown"));
        self.view.set_highlight_current_line(!markdown);
        self.buffer.place_cursor(&self.buffer.start_iter());
        self.update_margin();
    }

    fn guess_language(&self) -> Option<sourceview5::Language> {
        let lm = sourceview5::LanguageManager::default();
        let name = self.path.borrow().as_ref().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
        match name {
            Some(n) => lm.guess_language(Some(&n), None),
            None => lm.language("markdown"),
        }
    }

    /// Toggles between rich Markdown and raw source editing.
    pub fn toggle_source(&self) {
        let cur = self.cursor().offset();
        let text = self.content();
        let rich = !self.markdown.get();
        // Undo reaches across the switch: the rich history is kept while
        // the source shows, and edits made there become one step.
        let kept = if rich {
            self.rich_history.borrow_mut().take()
        } else {
            self.checkpoint();
            Some((self.history.borrow().clone(), self.hist_pos.get()))
        };
        self.set_content(&text, rich);
        if !rich {
            *self.rich_history.borrow_mut() = kept;
        } else if self.markdown.get()
            && let Some((h, pos)) = kept
        {
            *self.history.borrow_mut() = h;
            self.hist_pos.set(pos);
            self.hist_dirty.set(true);
            self.checkpoint();
        }
        let off = cur.min(self.buffer.char_count());
        self.buffer.place_cursor(&self.buffer.iter_at_offset(off));
        self.scroll_to_cursor();
        self.notify();
    }

    fn copy_markdown(&self, cut: bool) {
        let Some((s, e)) = self.buffer.selection_bounds() else { return };
        // A selected table is a block of its own, not inline text.
        let single = s.line() == e.line() && self.line_anchor(s.line()).is_none();
        let text = if single {
            model::serialize_line(&Line { block: Block::Para, spans: self.spans(s.offset(), e.offset()) })
        } else {
            model::serialize(&self.model_range(s.offset(), e.offset()))
        };
        self.view.clipboard().set_text(&text);
        if cut {
            self.buffer.begin_user_action();
            let (mut s, mut e) = (s, e);
            self.buffer.delete_interactive(&mut s, &mut e, true);
            self.buffer.end_user_action();
        }
    }

    /// Pastes clipboard text; Markdown is rendered unless `raw`.
    pub fn paste(self: &Rc<Self>, raw: bool) {
        let clipboard = self.view.clipboard();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(Some(text)) = clipboard.read_text_future().await else { return };
            let Some(t) = weak.upgrade() else { return };
            t.insert_text(&text, raw);
        });
    }

    pub fn insert_text(&self, text: &str, raw: bool) {
        self.checkpoint();
        let text = text.replace("\r\n", "\n").replace('\0', "␀");
        if raw || !self.markdown.get() {
            self.buffer.begin_user_action();
            self.buffer.delete_selection(true, true);
            self.buffer.insert_interactive_at_cursor(&text, true);
            self.buffer.end_user_action();
            self.scroll_to_cursor();
            return;
        }
        let lines = model::parse(&text);
        self.begin();
        self.buffer.delete_selection(true, true);
        let mut it = self.cursor();
        let first_line = it.line();
        let info = self.line_info(first_line);
        let line_empty = info.start == info.end;
        let simple = lines.len() == 1 && lines[0].block == Block::Para;
        if simple {
            // Inline paste takes the style of the insertion point.
            let start = it.offset();
            self.insert_lines(&mut it, &lines);
            let len = it.offset() - start;
            if lines[0].spans.iter().all(|s| s.style == Style::default()) {
                self.inherit(start, len);
            }
        } else {
            let mut lines = lines;
            // Mid-line paste: the first pasted line continues the current line.
            if !line_empty && matches!(lines[0].block, Block::Heading(_) | Block::Quote | Block::Bullet { .. } | Block::Numbered { .. } | Block::Check { .. }) {
                self.buffer.insert(&mut it, "\n");
            } else if !line_empty {
                lines[0].block = Block::Para;
            }
            self.insert_lines(&mut it, &lines);
        }
        let last_line = it.line();
        self.buffer.place_cursor(&it);
        self.normalize_lines(first_line, last_line, None);
        self.end();
        self.scroll_to_cursor();
        self.notify();
    }

    pub fn title(&self) -> String {
        if let Some(p) = self.path.borrow().as_ref() {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            return if self.readonly.get() { format!("{name} (lecture seule)") } else { name };
        }
        let mut it = self.buffer.start_iter();
        for _ in 0..50 {
            let mut e = it;
            if !e.ends_line() {
                e.forward_to_line_end();
            }
            let text = self.buffer.text(&it, &e, false);
            let t = text.trim_start_matches([' ', '\t', BULLET, UNCHECKED, CHECKED]).trim();
            if !t.is_empty() {
                let s: String = t.chars().take(28).collect();
                return if t.chars().count() > 28 { format!("{}…", s.trim_end()) } else { s };
            }
            if !it.forward_line() {
                break;
            }
        }
        crate::i18n::tr("New note").into()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.char_count() == 0
    }

    pub fn word_count(&self) -> usize {
        let (s, e) = self.buffer.bounds();
        self.buffer
            .text(&s, &e, false)
            .split_whitespace()
            .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
            .count()
    }

    pub fn restore_cursor(&self, off: i32) {
        let off = off.clamp(0, self.buffer.char_count());
        self.buffer.place_cursor(&self.buffer.iter_at_offset(off));
        let mark = self.buffer.get_insert();
        let view = self.view.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(60), move || {
            view.scroll_to_mark(&mark, 0.15, false, 0.0, 0.0);
        });
    }

    /// Records `text` as the file's content on disk.
    pub fn set_disk(&self, text: String) {
        let norm = if self.markdown.get() && self.csv.borrow().is_none() { model::serialize(&model::parse(&text)) } else { text.clone() };
        *self.saved.borrow_mut() = Some(norm);
        *self.disk.borrow_mut() = Some(text);
    }

    pub fn cursor_offset(&self) -> i32 {
        self.buffer.cursor_position()
    }
}

impl Tab {
    /// The table anchor on `line`, if any.
    fn line_anchor(&self, line: i32) -> Option<(i32, gtk::TextChildAnchor)> {
        if self.tables.borrow().is_empty() && self.graveyard.borrow().is_empty() {
            return None;
        }
        let (start, end) = self.line_bounds(line);
        let mut it = start;
        while it.offset() < end.offset() {
            if let Some(a) = it.child_anchor() {
                return Some((it.offset(), a));
            }
            it.forward_char();
        }
        None
    }

    /// Repositions the overlay tables on their placeholders after layout.
    pub fn schedule_table_sync(&self) {
        if self.sync_pending.get() || self.tables.borrow().is_empty() {
            return;
        }
        self.sync_pending.set(true);
        let me = self.me.borrow().clone();
        self.view.add_tick_callback(move |_, _| {
            if let Some(t) = me.upgrade() {
                t.sync_pending.set(false);
                t.sync_tables();
            }
            glib::ControlFlow::Break
        });
    }

    /// Minimap geometry: scale, and the map's offset when the document is
    /// taller than the map.
    fn map_frame(&self) -> (f64, f64) {
        let adj = self.scrolled.vadjustment();
        let column = (self.scrolled.width() - 2 * self.margin.get().max(0)).max(200) as f64;
        let k = ((self.map.width() as f64 - 10.0) / column).clamp(0.04, 0.14);
        let h = self.map.height() as f64;
        let content = adj.upper() * k;
        let room = (adj.upper() - adj.page_size()).max(1.0);
        let off = if content > h { (adj.value() / room).clamp(0.0, 1.0) * (content - h) } else { 0.0 };
        (k, off)
    }

    fn draw_minimap(&self, cr: &gtk::cairo::Context, width: i32, height: i32) {
        let [fg, muted, accent, band, _] = *self.map_colors.borrow();
        let set = |c: &gdk::RGBA, a: f64| cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, a);
        let (k, off) = self.map_frame();
        let (w, h) = (width as f64, height as f64);
        let x0 = 5.0;
        let cw = (self.view.create_pango_layout(Some("0000000000")).pixel_size().0 as f64 / 10.0).max(4.0);
        let column = (self.scrolled.width() - 2 * self.margin.get().max(0)).max(200) as f64;
        let per_row = (column / cw).max(10.0);
        let line_h = self.view.create_pango_layout(Some("Xg")).pixel_size().1.max(1) as f64;
        let adj = self.scrolled.vadjustment();
        let (y0, y1) = (off / k, (off + h) / k);

        {
            // Text, line by line, as bars.
            let (mut it, _) = self.view.line_at_y(y0 as i32);
            loop {
                let (ly, lh) = self.view.line_yrange(&it);
                let (ly, lh) = (ly as f64, lh as f64);
                if ly > y1 {
                    break;
                }
                let my = ly * k - off;
                let line = it.line();
                let info = self.line_info(line);
                if info.kind == Kind::Table {
                    if let Some((_, a)) = self.line_anchor(line)
                        && let Some(tv) = self.tables.borrow().iter().find(|s| s.anchor == a).map(|s| s.view.clone())
                    {
                        self.draw_table_mini(cr, &tv, x0, my, k, w - x0 - 3.0, line_h, cw);
                    }
                } else {
                    let text = self.text_range(info.start, info.end);
                    let kind = &info.kind;
                    let (alpha, thick, scale) = match kind {
                        Kind::Heading(_) => (0.9, 0.75, 1.4),
                        Kind::Code { fence: true } => (0.25, 0.4, 1.0),
                        Kind::Code { .. } => (0.4, 0.5, 1.0),
                        Kind::Quote => (0.5, 0.5, 1.0),
                        _ => (0.45, 0.5, 1.0),
                    };
                    if matches!(kind, Kind::Code { .. }) {
                        set(&band, 1.0);
                        cr.rectangle(x0 - 2.0, my, column * k + 4.0, (lh * k).max(1.0));
                        let _ = cr.fill();
                    }
                    if *kind == Kind::Quote {
                        set(&accent, 0.5);
                        cr.rectangle(x0 - 3.0, my, 1.5, (lh * k).max(1.0));
                        let _ = cr.fill();
                    }
                    let chars: Vec<char> = text.chars().collect();
                    let row_chars = (per_row / scale).max(5.0) as usize;
                    let rows = chars.len().div_ceil(row_chars).max(1);
                    let row_h = (lh * k) / rows as f64;
                    let cwm = cw * k * scale;
                    for (r, chunk) in chars.chunks(row_chars).enumerate() {
                        let mut col = 0usize;
                        while col < chunk.len() {
                            if chunk[col].is_whitespace() {
                                col += 1;
                                continue;
                            }
                            let start = col;
                            while col < chunk.len() && !chunk[col].is_whitespace() {
                                col += 1;
                            }
                            if r == 0 && info.is_list() && (start as i32) < info.prefix {
                                set(&accent, 0.85);
                            } else {
                                set(&fg, alpha);
                            }
                            let bh = (row_h * thick).max(1.0);
                            cr.rectangle(
                                x0 + start as f64 * cwm,
                                my + r as f64 * row_h + (row_h - bh) / 2.0,
                                ((col - start) as f64 * cwm).max(1.0),
                                bh,
                            );
                            let _ = cr.fill();
                        }
                    }
                }
                if !it.forward_line() {
                    break;
                }
            }
        }

        // Search hits, across the whole width.
        for (y, current) in self.hit_marks.borrow().iter() {
            let my = y * k - off;
            if my < -3.0 || my > h + 3.0 {
                continue;
            }
            set(&accent, if *current { 1.0 } else { 0.65 });
            let bh = if *current { 3.0 } else { 2.0 };
            cr.rectangle(0.0, (my - bh / 2.0).round(), w, bh);
            let _ = cr.fill();
        }

        // The visible part of the document.
        let vy = adj.value() * k - off;
        let vh = (adj.page_size() * k).max(8.0);
        set(&accent, 0.10);
        cr.rectangle(0.0, vy, w, vh);
        let _ = cr.fill();
        set(&muted, 0.35);
        cr.set_line_width(1.0);
        cr.rectangle(0.5, vy.round() + 0.5, w - 1.0, vh.round() - 1.0);
        let _ = cr.stroke();
    }

    /// A table in the minimap: frame, header band, text as bars.
    #[allow(clippy::too_many_arguments)]
    fn draw_table_mini(&self, cr: &gtk::cairo::Context, tv: &TableView, x0: f64, y0: f64, k: f64, room: f64, line_h: f64, cw: f64) {
        let (widths, heights, lens, _) = tv.geometry();
        let [fg, muted, _, band, _] = *self.map_colors.borrow();
        let set = |c: &gdk::RGBA, a: f64| cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, a);
        let total = widths.iter().sum::<i32>().max(1) as f64;
        let kx = (room / total).min(k);
        let mut y = y0;
        for (r, rh) in heights.iter().enumerate() {
            let rh = *rh as f64;
            if r == 0 {
                set(&band, 1.0);
                cr.rectangle(x0, y, total * kx, rh * k);
                let _ = cr.fill();
            }
            let mut x = x0;
            for (c, cwid) in widths.iter().enumerate() {
                let cwid = *cwid as f64;
                let len = lens.get(r).and_then(|l| l.get(c)).copied().unwrap_or(0) as f64;
                let per_line = ((cwid - 18.0) / cw).max(1.0);
                let lines = (len / per_line).ceil().min(((rh - 10.0) / line_h).max(1.0));
                set(&fg, if r == 0 { 0.7 } else { 0.45 });
                let mut rest = len;
                for i in 0..lines as usize {
                    let bar = rest.min(per_line) * cw;
                    rest -= per_line;
                    cr.rectangle(
                        x + 8.0 * kx,
                        y + (5.0 + i as f64 * line_h + line_h * 0.25) * k,
                        (bar * kx).max(1.0),
                        (line_h * 0.5 * k).max(1.0),
                    );
                }
                let _ = cr.fill();
                x += cwid * kx;
            }
            y += rh * k;
        }
        set(&muted, 0.6);
        cr.set_line_width(1.0);
        cr.rectangle(x0 + 0.5, y0 + 0.5, (total * kx - 1.0).max(1.0), (y - y0 - 1.0).max(1.0));
        let _ = cr.stroke();
    }

    /// Click or drag in the minimap scrolls the text.
    fn connect_minimap(self: &Rc<Self>) {
        let me = Rc::downgrade(self);
        self.map.set_draw_func(move |_, cr, w, h| {
            if let Some(t) = me.upgrade() {
                t.draw_minimap(cr, w, h);
            }
        });
        let adj = self.scrolled.vadjustment();
        let map = self.map.clone();
        adj.connect_value_changed(move |_| map.queue_draw());
        let map = self.map.clone();
        adj.connect_changed(move |_| map.queue_draw());

        let drag = gtk::GestureDrag::new();
        let start = Rc::new(Cell::new(0.0f64));
        let me = Rc::downgrade(self);
        let st = start.clone();
        drag.connect_drag_begin(move |_, _, y| {
            let Some(t) = me.upgrade() else { return };
            let adj = t.scrolled.vadjustment();
            let (k, off) = t.map_frame();
            let vy = adj.value() * k - off;
            let vh = adj.page_size() * k;
            if y < vy || y > vy + vh {
                // Outside the visible band: centre the text on that point.
                let target = (y + off) / k - adj.page_size() / 2.0;
                t.scroll_target.set(None);
                adj.set_value(target.clamp(adj.lower(), (adj.upper() - adj.page_size()).max(adj.lower())));
            }
            st.set(adj.value());
        });
        let me = Rc::downgrade(self);
        drag.connect_drag_update(move |_, _, dy| {
            let Some(t) = me.upgrade() else { return };
            let adj = t.scrolled.vadjustment();
            let (k, _) = t.map_frame();
            let h = t.map.height() as f64;
            let room = (adj.upper() - adj.page_size()).max(1.0);
            // How far the visible band moves in the map per pixel of text.
            let content = adj.upper() * k;
            let per_px = if content > h { (h - adj.page_size() * k) / room } else { k };
            let target = start.get() + dy / per_px.max(0.01);
            adj.set_value(target.clamp(adj.lower(), room));
        });
        self.map.add_controller(drag);
    }

    /// The caret never rests on a table's line (it would be as tall as the
    /// table): arrows enter the table, anything else moves past it.
    fn keep_caret_off_tables(&self) {
        let cur = self.cursor();
        let line = cur.line();
        let last = self.caret_line.replace(line);
        if self.busy.get() > 0 || self.buffer.has_selection() || self.tables.borrow().is_empty() {
            return;
        }
        let Some((_, anchor)) = self.line_anchor(line) else { return };
        let tv = self.tables.borrow().iter().find(|s| s.anchor == anchor).map(|s| s.view.clone());
        if self.view.has_focus()
            && let Some(tv) = tv
        {
            tv.enter(last > line);
            return;
        }
        let target = if line + 1 < self.buffer.line_count() {
            self.buffer.iter_at_line(line + 1).unwrap()
        } else if line > 0 {
            self.line_bounds(line - 1).1
        } else {
            return;
        };
        self.buffer.place_cursor(&target);
    }

    /// A list line's bullet, number or checkbox is not text: typing before
    /// it would turn the item into a paragraph starting with the glyph. The
    /// caret goes to the item's text, or, when stepping left from there, to
    /// the end of the previous line.
    fn keep_caret_off_markers(&self) {
        let cur = self.cursor();
        let off = cur.offset();
        let prev = self.caret_offset.replace(off);
        if self.busy.get() > 0 || !self.markdown.get() || self.buffer.has_selection() {
            return;
        }
        let info = self.line_info(cur.line());
        let content = info.start + info.prefix;
        if !info.is_list() || off >= content {
            return;
        }
        let target = if prev == content && off == content - 1 && cur.line() > 0 {
            self.line_bounds(cur.line() - 1).1
        } else {
            self.buffer.iter_at_offset(content)
        };
        self.caret_offset.set(target.offset());
        self.buffer.place_cursor(&target);
    }

    /// Width for tables: the text column, and the whole view when wide.
    fn table_space(&self) -> (i32, i32) {
        let width = self.scrolled.width().max(300);
        let m = self.margin.get().max(0);
        ((width - 2 * m).max(160), (width - 32).max(160))
    }

    fn sync_tables(&self) {
        let mut changed = false;
        let (normal, wide) = self.table_space();
        for slot in self.tables.borrow().iter() {
            slot.view.set_available(normal, wide);
            let root = &slot.view.root;
            let (_, w, _, _) = root.measure(gtk::Orientation::Horizontal, -1);
            let w = w.min(slot.view.max_width());
            let (_, h, _, _) = root.measure(gtk::Orientation::Vertical, w);
            // The placeholder never widens the text view: a wide table
            // overflows the column in the overlay only.
            let pw = w.min(normal);
            if slot.placeholder.width_request() != pw || slot.placeholder.height_request() != h {
                slot.placeholder.set_size_request(pw, h);
                changed = true;
            }
            slot.width.set(w);
            root.set_visible(true);
        }
        self.overlay.queue_allocate();
        self.map.queue_draw();
        // Wrapped cell heights settle a few frames after a width change:
        // measure again until nothing moves.
        let passes = self.sync_passes.get();
        if changed && passes < 6 {
            self.sync_passes.set(passes + 1);
            let me = self.me.borrow().clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(40), move || {
                if let Some(t) = me.upgrade() {
                    t.sync_tables();
                }
            });
        } else {
            self.sync_passes.set(0);
        }
    }

    /// Where the overlay draws `child`: on top of its placeholder.
    fn table_position(&self, child: &gtk::Widget) -> Option<gdk::Rectangle> {
        let tables = self.tables.borrow();
        let slot = tables.iter().find(|s| s.view.root.upcast_ref::<gtk::Widget>() == child)?;
        let w = slot.width.get().max(1);
        let h = slot.placeholder.height_request().max(1);
        // From the anchor's place in the text layout, not from the
        // placeholder's allocation: GtkTextView allocates child widgets only
        // once their line is validated, which lags behind a fast scroll.
        if slot.anchor.is_deleted() {
            return Some(gdk::Rectangle::new(0, -100_000, w, h));
        }
        let iter = self.buffer.iter_at_child_anchor(&slot.anchor);
        let rect = self.view.iter_location(&iter);
        let (vx, vy) = self.view.buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y());
        let Some(p) = self.view.compute_point(&self.overlay, &gtk::graphene::Point::new(vx as f32, vy as f32)) else {
            return Some(gdk::Rectangle::new(0, -100_000, w, h));
        };
        let mut x = p.x().round() as i32;
        if w > slot.placeholder.width_request() {
            // Wider than the column: centre it in the view.
            x = ((self.overlay.width() - w) / 2).max(16).min(x);
        }
        Some(gdk::Rectangle::new(x, p.y().round() as i32, w, h))
    }

    /// Document-order indices of the tables shown wide.
    pub fn wide_tables(&self) -> Vec<usize> {
        let mut v: Vec<(i32, bool)> = self
            .tables
            .borrow()
            .iter()
            .map(|s| (self.buffer.iter_at_child_anchor(&s.anchor).offset(), s.view.is_wide()))
            .collect();
        v.sort();
        v.iter().enumerate().filter(|(_, (_, w))| *w).map(|(i, _)| i).collect()
    }

    pub fn set_wide_tables(&self, wide: &[usize]) {
        let mut v: Vec<(i32, Rc<TableView>)> = self
            .tables
            .borrow()
            .iter()
            .map(|s| (self.buffer.iter_at_child_anchor(&s.anchor).offset(), s.view.clone()))
            .collect();
        v.sort_by_key(|(o, _)| *o);
        for (i, (_, tv)) in v.iter().enumerate() {
            tv.set_wide(wide.contains(&i));
        }
        self.schedule_table_sync();
    }

    /// Keeps a table alone on its line; returns true if it moved text.
    fn isolate_table_line(&self, line: i32) -> bool {
        let Some((a, _)) = self.line_anchor(line) else { return false };
        let (start, end) = self.line_bounds(line);
        let mut moved = false;
        if a + 1 < end.offset() {
            let mut it = self.buffer.iter_at_offset(a + 1);
            self.buffer.insert(&mut it, "\n");
            moved = true;
        }
        if a > start.offset() {
            let mut it = self.buffer.iter_at_offset(a);
            self.buffer.insert(&mut it, "\n");
            moved = true;
        }
        moved
    }

    fn insert_table_at(&self, it: &mut gtk::TextIter, table: Table) -> Rc<TableView> {
        let anchor = self.buffer.create_child_anchor(it);
        let tv = TableView::new(table);
        if let Some(c) = self.cell_colors.get() {
            tv.set_colors(c);
        }
        let me = self.me.borrow().clone();
        *tv.on_change.borrow_mut() = Some(Rc::new({
            let me = me.clone();
            move || {
                if let Some(t) = me.upgrade() {
                    t.notify();
                    t.schedule_table_sync();
                }
            }
        }));
        let a = anchor.clone();
        *tv.on_exit.borrow_mut() = Some(Rc::new({
            let me = me.clone();
            move |after| {
                if let Some(t) = me.upgrade() {
                    t.leave_table(&a, after);
                }
            }
        }));
        *tv.on_resize.borrow_mut() = Some(Rc::new({
            let me = me.clone();
            move || {
                if let Some(t) = me.upgrade() {
                    t.notify();
                    t.schedule_table_sync();
                }
            }
        }));
        *tv.on_undo.borrow_mut() = Some(Rc::new({
            let me = me.clone();
            move |redo| {
                if let Some(t) = me.upgrade() {
                    if redo {
                        t.redo();
                    } else {
                        t.undo();
                    }
                    t.view.grab_focus();
                }
            }
        }));
        let a = anchor.clone();
        *tv.on_select_all.borrow_mut() = Some(Rc::new({
            let me = me.clone();
            move || {
                if let Some(t) = me.upgrade() {
                    t.select_table(&a);
                }
            }
        }));
        // Editing a cell drops the text view's selection, as a click would.
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter({
            let me = me.clone();
            move |_| {
                if let Some(t) = me.upgrade()
                    && t.buffer.has_selection()
                {
                    t.buffer.place_cursor(&t.buffer.iter_at_mark(&t.buffer.get_insert()));
                }
            }
        });
        tv.root.add_controller(focus);
        let a = anchor.clone();
        *tv.on_delete.borrow_mut() = Some(Rc::new(move || {
            if let Some(t) = me.upgrade() {
                t.delete_table(&a);
            }
        }));
        let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // Never under the pointer: unmapping a hovered child while its anchor
        // is being deleted makes GTK read the half-updated buffer (segfault).
        placeholder.set_can_target(false);
        self.view.add_child_at_anchor(&placeholder, &anchor);
        let (normal, wide) = self.table_space();
        tv.set_available(normal, wide);
        tv.root.set_visible(false);
        self.overlay.add_overlay(&tv.root);
        self.overlay.set_clip_overlay(&tv.root, true);
        // Touchpad scrolling over a table moves the page (the mouse wheel is
        // handled by the overlay, smoothly).
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let adj = self.scrolled.vadjustment();
        scroll.connect_scroll(move |_, _, dy| {
            adj.set_value(adj.value() + dy);
            glib::Propagation::Stop
        });
        tv.root.add_controller(scroll);
        self.tables.borrow_mut().push(TableSlot { anchor, view: tv.clone(), placeholder, width: Cell::new(1) });
        self.schedule_table_sync();
        tv
    }

    fn leave_table(&self, anchor: &gtk::TextChildAnchor, after: bool) {
        let off = self.buffer.iter_at_child_anchor(anchor).offset();
        let line = self.buffer.iter_at_offset(off).line();
        self.begin();
        if after {
            if line + 1 >= self.buffer.line_count() {
                let mut it = self.buffer.iter_at_offset(off + 1);
                self.buffer.insert(&mut it, "\n");
            }
            let s = self.buffer.iter_at_line(line + 1).unwrap_or_else(|| self.buffer.end_iter());
            self.buffer.place_cursor(&s);
        } else {
            if line == 0 {
                let mut it = self.buffer.iter_at_offset(off);
                self.buffer.insert(&mut it, "\n");
                self.buffer.place_cursor(&self.buffer.start_iter());
            } else {
                let (_, e) = self.line_bounds(line - 1);
                self.buffer.place_cursor(&e);
            }
        }
        self.end();
        self.view.grab_focus();
        self.scroll_to_cursor();
    }

    /// Selects the whole table in the text view (its anchor character), so
    /// that copy, cut and delete act on it and Ctrl+A goes on to the document.
    fn select_table(&self, anchor: &gtk::TextChildAnchor) {
        if anchor.is_deleted() {
            return;
        }
        if let Some(slot) = self.tables.borrow().iter().find(|s| s.anchor == *anchor) {
            slot.view.clear_selection();
        }
        let off = self.buffer.iter_at_child_anchor(anchor).offset();
        self.view.grab_focus();
        self.buffer.select_range(&self.buffer.iter_at_offset(off + 1), &self.buffer.iter_at_offset(off));
        self.update_table_selection();
    }

    /// Draws as selected the tables inside the text view's selection.
    fn update_table_selection(&self) {
        let sel = self.buffer.selection_bounds().map(|(s, e)| (s.offset(), e.offset()));
        for slot in self.tables.borrow().iter() {
            let on = !slot.anchor.is_deleted()
                && sel.is_some_and(|(s, e)| {
                    let off = self.buffer.iter_at_child_anchor(&slot.anchor).offset();
                    s <= off && off < e
                });
            slot.view.set_selected(on);
        }
    }

    /// Backspace or Delete on a selection that is exactly one table: delete
    /// it with its line, as the table's own button does.
    fn delete_selected_table(&self) -> bool {
        let Some((s, e)) = self.buffer.selection_bounds() else { return false };
        if e.offset() != s.offset() + 1 {
            return false;
        }
        let Some(anchor) = s.child_anchor() else { return false };
        self.delete_table(&anchor);
        true
    }

    fn delete_table(&self, anchor: &gtk::TextChildAnchor) {
        let off = self.buffer.iter_at_child_anchor(anchor).offset();
        let it = self.buffer.iter_at_offset(off + 1);
        let mut end = if it.ends_line() && !it.is_end() { off + 2 } else { off + 1 };
        // Between two blank lines, keep only one.
        let before_blank = off > 0 && self.buffer.iter_at_offset(off - 1).starts_line();
        let after = self.buffer.iter_at_offset(end);
        if end == off + 2 && before_blank && after.ends_line() && !after.is_end() {
            end += 1;
        }
        self.begin();
        self.delete_range(off, end);
        self.end();
        self.view.grab_focus();
        self.notify();
    }

    /// Re-creates tables whose anchor an undo brought back as plain text.
    fn restore_tables(&self) {
        if self.graveyard.borrow().is_empty() {
            return;
        }
        let mut offsets = Vec::new();
        let mut it = self.buffer.start_iter();
        loop {
            if it.char() == '\u{fffc}' && it.child_anchor().is_none() {
                offsets.push(it.offset());
            }
            if !it.forward_char() {
                break;
            }
        }
        if offsets.is_empty() {
            return;
        }
        self.busy.set(self.busy.get() + 1);
        for off in offsets.into_iter().rev() {
            let Some(table) = self.graveyard.borrow_mut().pop() else { break };
            self.delete_range(off, off + 1);
            let mut it = self.buffer.iter_at_offset(off);
            self.insert_table_at(&mut it, table);
        }
        self.busy.set(self.busy.get() - 1);
        self.notify();
    }

    /// Inserts an empty table at the cursor (toolbar, Ctrl+Alt+T).
    pub fn insert_table(&self) {
        if !self.markdown.get() {
            return;
        }
        self.checkpoint();
        let line = self.cursor().line();
        let info = self.line_info(line);
        self.begin();
        let at = if info.start == info.end && info.kind == Kind::Para {
            info.start
        } else {
            let mut it = self.buffer.iter_at_offset(info.end);
            self.buffer.insert(&mut it, "\n");
            info.end + 1
        };
        let l = self.buffer.iter_at_offset(at).line();
        self.set_line_block(l, None);
        let mut it = self.buffer.iter_at_offset(at);
        let tv = self.insert_table_at(&mut it, Table::new(2, 2));
        if it.is_end() {
            self.buffer.insert(&mut it, "\n");
        }
        self.end();
        self.notify();
        glib::idle_add_local_once(move || tv.focus_cell(0, 0));
    }

    /// `| a | b |` then `|---|---|` + Enter becomes a table.
    fn table_from_typed_head(&self, line: i32) -> bool {
        let prev = self.line_info(line - 1);
        let cur = self.line_info(line);
        if prev.kind != Kind::Para {
            return false;
        }
        let head = self.text_range(prev.start, prev.end);
        let delim = self.text_range(cur.start, cur.end);
        let Some(mut table) = model::table_head(&head, &delim) else { return false };
        table.rows.push(vec![String::new(); table.cols()]);
        self.begin();
        self.delete_range(prev.start, cur.end);
        let mut it = self.buffer.iter_at_offset(prev.start);
        let tv = self.insert_table_at(&mut it, table);
        if it.is_end() {
            self.buffer.insert(&mut it, "\n");
        }
        self.end();
        self.notify();
        glib::idle_add_local_once(move || tv.focus_cell(1, 0));
        true
    }

    fn handle_delete(&self) -> bool {
        if self.buffer.has_selection() {
            return self.delete_selected_table();
        }
        let cur = self.cursor();
        if cur.child_anchor().is_some() {
            let mut e = cur;
            e.forward_char();
            self.buffer.select_range(&cur, &e);
            return true;
        }
        false
    }

    /// Every occurrence of `needle`, text and table cells, in document order.
    pub fn find(&self, needle: &str) -> Vec<Hit> {
        let (s, e) = self.buffer.bounds();
        // `slice` keeps the anchor characters, so offsets match the buffer.
        let text = self.buffer.slice(&s, &e, true);
        let mut hits: Vec<Hit> = crate::find::find_all(&text, needle)
            .into_iter()
            .map(|(a, b)| Hit { table: None, start: a as i32, end: b as i32, key: (a as i32, 0, 0, 0) })
            .collect();
        for slot in self.tables.borrow().iter() {
            if slot.anchor.is_deleted() {
                continue;
            }
            let off = self.buffer.iter_at_child_anchor(&slot.anchor).offset();
            for (r, row) in slot.view.cell_texts().iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    for (a, b) in crate::find::find_all(cell, needle) {
                        hits.push(Hit {
                            table: Some((slot.anchor.clone(), r, c)),
                            start: a as i32,
                            end: b as i32,
                            key: (off, r + 1, c, a as i32),
                        });
                    }
                }
            }
        }
        hits.sort_by_key(|h| h.key);
        hits
    }

    /// Marks hits; `current` stands out.
    pub fn show_hits(&self, hits: &[Hit], current: Option<usize>) {
        let (s, e) = self.buffer.bounds();
        self.buffer.remove_tag_by_name("find", &s, &e);
        self.buffer.remove_tag_by_name("findcur", &s, &e);
        let mut per_table: Vec<(gtk::TextChildAnchor, Vec<CellMark>)> = Vec::new();
        for (i, h) in hits.iter().enumerate() {
            let cur = current == Some(i);
            match &h.table {
                None => self.buffer.apply_tag_by_name(
                    if cur { "findcur" } else { "find" },
                    &self.buffer.iter_at_offset(h.start),
                    &self.buffer.iter_at_offset(h.end),
                ),
                Some((a, r, c)) => match per_table.iter_mut().find(|(x, _)| x == a) {
                    Some((_, v)) => v.push((*r, *c, h.start, h.end, cur)),
                    None => per_table.push((a.clone(), vec![(*r, *c, h.start, h.end, cur)])),
                },
            }
        }
        // Positions for the minimap.
        let mut marks = Vec::with_capacity(hits.len());
        for (i, h) in hits.iter().enumerate() {
            let y = match &h.table {
                None => self.view.iter_location(&self.buffer.iter_at_offset(h.start)).y() as f64,
                Some((a, r, c)) => {
                    let base = self.view.iter_location(&self.buffer.iter_at_child_anchor(a)).y() as f64;
                    base + self.tables.borrow().iter().find(|s| s.anchor == *a).map_or(0.0, |s| s.view.cell_y(*r, *c))
                }
            };
            marks.push((y, current == Some(i)));
        }
        *self.hit_marks.borrow_mut() = marks;
        self.map.queue_draw();
        let colors = *self.find_colors.borrow();
        for slot in self.tables.borrow().iter() {
            let marks = per_table.iter().find(|(a, _)| *a == slot.anchor).map(|(_, v)| v.as_slice()).unwrap_or(&[]);
            slot.view.highlight(marks, &colors);
        }
    }

    pub fn clear_hits(&self) {
        self.show_hits(&[], None);
    }

    /// Scrolls so that the hit is in view, a third from the top.
    pub fn reveal_hit(&self, hit: &Hit) {
        let adj = self.scrolled.vadjustment();
        let y = match &hit.table {
            None => {
                let it = self.buffer.iter_at_offset(hit.start);
                let r = self.view.iter_location(&it);
                // Already visible: do not move.
                if (r.y() as f64) >= adj.value() + 20.0 && ((r.y() + r.height()) as f64) <= adj.value() + adj.page_size() - 20.0 {
                    return;
                }
                r.y() as f64
            }
            Some((a, row, col)) => {
                let it = self.buffer.iter_at_child_anchor(a);
                let base = self.view.iter_location(&it).y() as f64;
                let cell = self.tables.borrow().iter().find(|s| s.anchor == *a).map_or(0.0, |s| s.view.cell_y(*row, *col));
                let y = base + cell;
                if y >= adj.value() + 20.0 && y + 40.0 <= adj.value() + adj.page_size() - 20.0 {
                    return;
                }
                y
            }
        };
        self.scroll_target.set(None);
        adj.set_value((y - adj.page_size() / 3.0).clamp(adj.lower(), (adj.upper() - adj.page_size()).max(adj.lower())));
    }

    /// Leaves the cursor on the hit (search closed on it).
    pub fn select_hit(&self, hit: &Hit) {
        match &hit.table {
            None => {
                self.buffer.select_range(&self.buffer.iter_at_offset(hit.start), &self.buffer.iter_at_offset(hit.end));
                self.view.grab_focus();
            }
            Some((a, r, c)) => {
                if let Some(slot) = self.tables.borrow().iter().find(|s| s.anchor == *a) {
                    slot.view.select_in_cell(*r, *c, hit.start, hit.end);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Undo: the history holds Markdown, so undoing restores text and
    // formatting together.
    // -----------------------------------------------------------------------

    fn reset_history(&self) {
        if let Some(id) = self.hist_timer.borrow_mut().take() {
            id.remove();
        }
        let first = if self.markdown.get() { self.to_markdown() } else { String::new() };
        *self.history.borrow_mut() = vec![(first, self.cursor_offset())];
        self.hist_pos.set(0);
        self.hist_dirty.set(true);
    }

    /// Records the current state if it changed since the last record.
    pub fn checkpoint(&self) {
        if let Some(id) = self.hist_timer.borrow_mut().take() {
            id.remove();
        }
        if !self.markdown.get() || self.restoring.get() {
            return;
        }
        self.push_step(self.to_markdown(), self.cursor_offset());
    }

    /// Records a state unless it is the current step.
    fn push_step(&self, md: String, cursor: i32) {
        if !self.markdown.get() || self.restoring.get() {
            return;
        }
        let mut h = self.history.borrow_mut();
        let pos = self.hist_pos.get();
        if h.get(pos).is_some_and(|(m, _)| *m == md) {
            return;
        }
        h.truncate(pos + 1);
        h.push((md, cursor));
        // Bounded: 300 steps or ~32 MB of text.
        while h.len() > 300 || (h.len() > 2 && h.iter().map(|(m, _)| m.len()).sum::<usize>() > 32 << 20) {
            h.remove(0);
        }
        self.hist_pos.set(h.len() - 1);
        self.hist_dirty.set(true);
    }

    /// Typing records a step after a short pause (and at each word, see
    /// `after_user_edit`).
    fn schedule_checkpoint(&self) {
        if !self.markdown.get() || self.restoring.get() {
            return;
        }
        if let Some(id) = self.hist_timer.borrow_mut().take() {
            id.remove();
        }
        let me = self.me.borrow().clone();
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
            if let Some(t) = me.upgrade() {
                t.hist_timer.borrow_mut().take();
                t.checkpoint();
            }
        });
        *self.hist_timer.borrow_mut() = Some(id);
    }

    pub fn undo(&self) {
        self.checkpoint();
        let pos = self.hist_pos.get();
        if pos > 0 {
            self.restore_step(pos - 1);
        }
    }

    pub fn redo(&self) {
        self.checkpoint();
        let pos = self.hist_pos.get();
        if pos + 1 < self.history.borrow().len() {
            self.restore_step(pos + 1);
        }
    }

    fn restore_step(&self, i: usize) {
        let Some((md, _)) = self.history.borrow().get(i).cloned() else { return };
        self.restoring.set(true);
        let adj = self.scrolled.vadjustment();
        let scroll = adj.value();
        let wide = self.wide_tables();
        let (s, e) = self.buffer.bounds();
        let old: Vec<char> = self.buffer.text(&s, &e, true).chars().collect();
        let old_cursor = self.cursor_offset();
        self.render_content(&md, true);
        self.set_wide_tables(&wide);
        self.hist_pos.set(i);
        self.hist_dirty.set(true);
        // The caret goes to where the text changed, not to where it was
        // when that state was recorded; a change of formatting only leaves
        // it in place.
        let (s, e) = self.buffer.bounds();
        let new: Vec<char> = self.buffer.text(&s, &e, true).chars().collect();
        let off = if old == new { old_cursor } else { change_end(&old, &new) as i32 };
        let off = off.clamp(0, self.buffer.char_count());
        self.buffer.place_cursor(&self.buffer.iter_at_offset(off));
        adj.set_value(scroll);
        // Same scroll as before, unless the change is off screen.
        let (view, mark) = (self.view.clone(), self.buffer.get_insert());
        glib::idle_add_local_once(move || {
            adj.set_value(scroll);
            view.scroll_mark_onscreen(&mark);
        });
        self.restoring.set(false);
        self.notify();
    }

    /// The Markdown undo history and its current step, to save it; empty
    /// for plain text (GTK's own history, which is not kept).
    pub fn with_history<R>(&self, f: impl FnOnce(&[(String, i32)], usize) -> R) -> R {
        if self.markdown.get() {
            f(&self.history.borrow(), self.hist_pos.get())
        } else if let Some((h, pos)) = self.rich_history.borrow().as_ref() {
            f(h, *pos)
        } else {
            f(&[], 0)
        }
    }

    /// Puts back a saved history. The current text becomes a step on top
    /// when it differs from the saved current step (the file changed in
    /// between): undo then goes back to before that change.
    pub fn restore_history(&self, steps: Vec<(String, i32)>, pos: usize) {
        if steps.is_empty() || self.readonly.get() {
            return;
        }
        let pos = pos.min(steps.len() - 1);
        if self.markdown.get() {
            *self.history.borrow_mut() = steps;
            self.hist_pos.set(pos);
            self.checkpoint();
        } else if self.path.borrow().as_deref().is_none_or(|p| {
            matches!(p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(), Some("md" | "markdown" | "mdown" | "mkd"))
                || crate::csv::is_csv_path(p)
        }) {
            // Shown as source: the rich view gets it back.
            *self.rich_history.borrow_mut() = Some((steps, pos));
        }
        self.hist_dirty.set(false);
    }

    pub fn set_readonly(&self, ro: bool) {
        self.readonly.set(ro);
        self.view.set_editable(!ro);
    }
}

/// Where the changed region ends in `new`, compared with `old`.
fn change_end(old: &[char], new: &[char]) -> usize {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old.iter().rev().zip(new.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    new.len() - suffix
}

/// Keeps fenced code consistent after free editing (deleted fences etc.).
fn fix_fences(lines: &mut Vec<Line>) {
    let mut out = Vec::with_capacity(lines.len() + 2);
    let mut in_code = false;
    for line in lines.drain(..) {
        match line.block {
            Block::Fence => {
                in_code = !in_code;
                out.push(line);
            }
            Block::Code => {
                if !in_code {
                    out.push(Line::plain(Block::Fence, "```"));
                    in_code = true;
                }
                out.push(line);
            }
            _ => {
                if in_code {
                    out.push(Line::plain(Block::Fence, "```"));
                    in_code = false;
                }
                out.push(line);
            }
        }
    }
    if in_code {
        out.push(Line::plain(Block::Fence, "```"));
    }
    *lines = out;
}

pub fn launch_uri(url: &str) {
    let launcher = gtk::UriLauncher::new(url);
    launcher.launch(None::<&gtk::Window>, gio::Cancellable::NONE, |r| {
        if let Err(e) = r {
            eprintln!("calepin: cannot open link: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::change_end;

    fn end(a: &str, b: &str) -> usize {
        change_end(&a.chars().collect::<Vec<_>>(), &b.chars().collect::<Vec<_>>())
    }

    #[test]
    fn caret_goes_to_the_change() {
        assert_eq!(end("abc Alpha", "abc "), 4); // insertion undone
        assert_eq!(end("abc ", "abc Alpha"), 9); // redone
        assert_eq!(end("xaay", "xay"), 2); // repeated letters
        assert_eq!(end("Titre\nfin", "Titre\nXfin"), 7);
    }
}
