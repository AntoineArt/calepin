//! Main window: tab bar, formatting toolbar, find bar, shortcuts, session.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk::{gdk, gio, glib, prelude::*};
use sourceview5::prelude::*;

use crate::editor::{BlockCmd, Tab, TabRef};
use crate::session::{self, Session, TabState};
use crate::theme::{self, Theme};

pub const APP_ID: &str = "io.github.antoineart.Calepin";

struct Header {
    root: gtk::Box,
    label: gtk::Label,
    /// Shown for tabs saved to a file; accent when modified.
    file: gtk::Image,
    dirty: gtk::Label,
}

/// Note never saved: no mark. File saved: file icon. File with unsaved
/// changes: accented icon and a dot.
fn show_state(h: &Header, path: Option<&Path>, dirty: bool) {
    h.file.set_visible(path.is_some());
    h.dirty.set_visible(path.is_some() && dirty);
    if dirty {
        h.file.add_css_class("modified");
    } else {
        h.file.remove_css_class("modified");
    }
    let tip = match path {
        None => "Note non enregistrée dans un fichier (conservée par Calepin)".to_string(),
        Some(p) if dirty => format!("{}\nModifié, non enregistré (Ctrl+S)", p.display()),
        Some(p) => format!("{}\nEnregistré", p.display()),
    };
    if h.root.tooltip_text().as_deref() != Some(tip.as_str()) {
        h.root.set_tooltip_text(Some(&tip));
    }
}

pub struct Window {
    pub win: gtk::ApplicationWindow,
    tabs: RefCell<Vec<TabRef>>,
    headers: RefCell<Vec<Header>>,
    active: Cell<usize>,
    stack: gtk::Stack,
    tabbar: gtk::Box,
    tab_scroller: gtk::ScrolledWindow,
    toolbar: gtk::Box,
    rich_buttons: RefCell<Vec<gtk::Widget>>,
    update_bar: gtk::Revealer,
    update_label: gtk::Label,
    update_button: RefCell<gtk::Button>,
    /// Rendered/raw Markdown switch: only for Markdown tabs.
    source_button: RefCell<Option<gtk::Button>>,
    count: gtk::Label,
    find_revealer: gtk::Revealer,
    find_entry: gtk::SearchEntry,
    find_label: gtk::Label,
    find_hits: RefCell<Vec<crate::editor::Hit>>,
    find_cur: Cell<Option<usize>>,
    find_origin: Cell<(i32, usize, usize, i32)>,
    /// Tab whose hits are marked.
    find_tab: RefCell<Option<String>>,
    find_timer: RefCell<Option<glib::SourceId>>,
    search_settings: sourceview5::SearchSettings,
    // Shared by all windows.
    session: Rc<RefCell<Session>>,
    theme: Rc<RefCell<Theme>>,
    css: gtk::CssProvider,
    scheme: Rc<RefCell<Option<sourceview5::StyleScheme>>>,
    generation: Rc<Cell<u32>>,
    /// A window holding tabs dragged out of the notes window.
    pub detached: bool,
    flush_timer: RefCell<Option<glib::SourceId>>,
    changed: RefCell<HashSet<String>>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
}

pub type WindowRef = Rc<Window>;

thread_local! {
    /// Strong: a detached window has no other owner on the Rust side.
    static WINDOWS: RefCell<Vec<WindowRef>> = const { RefCell::new(Vec::new()) };
    /// A Calepin window took the dragged tab. Wayland still reports the
    /// drag as cancelled (the payload never leaves the process).
    static TAB_DROPPED: Cell<bool> = const { Cell::new(false) };
}

/// Every open window, the notes window first.
pub fn all_windows() -> Vec<WindowRef> {
    let mut v: Vec<WindowRef> = WINDOWS.with(|w| w.borrow().clone());
    v.sort_by_key(|w| w.detached);
    v
}

fn main_window() -> Option<WindowRef> {
    all_windows().into_iter().find(|w| !w.detached)
}

fn window_of_tab(id: &str) -> Option<(WindowRef, usize)> {
    all_windows().into_iter().find_map(|w| {
        let i = w.tabs.borrow().iter().position(|t| t.id == id)?;
        Some((w, i))
    })
}

/// A tiling compositor places and closes windows itself: no title bar.
/// `CALEPIN_DECORATIONS=1` or `0` forces the choice.
pub fn tiling_wm() -> bool {
    match std::env::var("CALEPIN_DECORATIONS").as_deref() {
        Ok("1") => return false,
        Ok("0") => return true,
        _ => {}
    }
    ["HYPRLAND_INSTANCE_SIGNATURE", "SWAYSOCK", "NIRI_SOCKET", "I3SOCK"].iter().any(|v| std::env::var_os(v).is_some())
        || std::env::var("XDG_CURRENT_DESKTOP")
            .map(|d| ["hyprland", "sway", "niri", "i3", "river", "qtile", "dwl"].iter().any(|t| d.to_lowercase().contains(t)))
            .unwrap_or(false)
}

/// Runs a host command; inside Flatpak through `flatpak-spawn --host`.
pub fn host_command(args: &[&str]) -> std::process::Command {
    if std::env::var_os("FLATPAK_ID").is_some() {
        let mut c = std::process::Command::new("flatpak-spawn");
        c.arg("--host").args(args);
        c
    } else {
        let mut c = std::process::Command::new(args[0]);
        c.args(&args[1..]);
        c
    }
}

/// Hides the special workspace shown on the focused monitor (the SUPER+N
/// notes), so that a new window lands on the regular workspace.
fn leave_special_workspace() {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    let Ok(out) = host_command(&["hyprctl", "-j", "monitors"]).output() else { return };
    let Ok(mons) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return };
    let Some(mons) = mons.as_array() else { return };
    for m in mons {
        if m["focused"].as_bool() != Some(true) {
            continue;
        }
        if let Some(name) = m["specialWorkspace"]["name"].as_str().and_then(|n| n.strip_prefix("special:")) {
            let expr = format!("hl.dsp.workspace.toggle_special(\"{name}\")");
            let _ = host_command(&["hyprctl", "dispatch", &expr]).output();
        }
    }
}
type Command = Rc<dyn Fn(&Rc<Window>)>;

fn is_markdown_path(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("md" | "markdown" | "mdown" | "mkd")
    )
}

/// Text of a file; binary or non-UTF-8 content opens read-only so that saving
/// can never corrupt it.
fn decode(bytes: &[u8]) -> (String, bool) {
    const MAX: usize = 16 << 20;
    const PREVIEW: usize = 1 << 20;
    if bytes.len() > MAX {
        let mut s = String::from_utf8_lossy(&bytes[..PREVIEW]).replace('\0', "␀");
        s.push_str("\n\n… (fichier tronqué : trop gros pour être édité)");
        return (s, true);
    }
    let binary = bytes[..bytes.len().min(8192)].contains(&0);
    match std::str::from_utf8(bytes) {
        Ok(s) if !binary => (s.to_string(), false),
        // GTK takes no NUL bytes; show them as a symbol.
        _ => (String::from_utf8_lossy(bytes).replace('\0', "␀"), true),
    }
}

fn btn(label: &str, tooltip: &str, markup: bool) -> gtk::Button {
    let l = gtk::Label::new(None);
    if markup {
        l.set_markup(label);
    } else {
        l.set_text(label);
    }
    let b = gtk::Button::builder().child(&l).tooltip_text(tooltip).focus_on_click(false).build();
    b.set_can_focus(false);
    b
}

fn sep() -> gtk::Separator {
    let s = gtk::Separator::new(gtk::Orientation::Vertical);
    s.add_css_class("sep");
    s
}

impl Window {
    /// The notes window (SUPER+N): restores the session.
    pub fn new(app: &gtk::Application) -> WindowRef {
        let w = Self::build(app, None);
        crate::mark("widgets + css + scheme");
        w.restore();
        crate::mark("tabs restored");
        w
    }

    /// A window for tabs dragged out of another one.
    fn new_detached(from: &Window) -> WindowRef {
        let app = from.win.application().and_downcast::<gtk::Application>().expect("application");
        Self::build(&app, Some(from))
    }

    fn build(app: &gtk::Application, shared: Option<&Window>) -> WindowRef {
        let detached = shared.is_some();
        let (theme, session, css, scheme, generation) = match shared {
            Some(o) => (o.theme.clone(), o.session.clone(), o.css.clone(), o.scheme.clone(), o.generation.clone()),
            None => {
                let theme = Theme::load();
                crate::mark("theme loaded");
                session::migrate();
                let session = session::load();
                let css = gtk::CssProvider::new();
                gtk::style_context_add_provider_for_display(
                    &gdk::Display::default().expect("display"),
                    &css,
                    gtk::STYLE_PROVIDER_PRIORITY_USER,
                );
                (Rc::new(RefCell::new(theme)), Rc::new(RefCell::new(session)), css, Rc::new(RefCell::new(None)), Rc::new(Cell::new(0)))
            }
        };

        let win = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Calepin")
            .default_width(900)
            .default_height(640)
            .decorated(!tiling_wm())
            .build();
        win.add_css_class("calepin");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("mw-root");

        // Tab bar
        let tabbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let tab_scroller = gtk::ScrolledWindow::builder()
            .child(&tabbar)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .build();
        tab_scroller.set_propagate_natural_height(true);
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("tabbar");
        bar.append(&tab_scroller);
        let new_btn = gtk::Button::with_label("+");
        new_btn.add_css_class("newtab");
        new_btn.set_tooltip_text(Some("Nouvel onglet (Ctrl+T)"));
        new_btn.set_can_focus(false);
        new_btn.set_valign(gtk::Align::End);
        bar.append(&new_btn);
        let bar_wrap = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar_wrap.append(&bar);
        bar.set_hexpand(true);

        // Toolbar
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        toolbar.add_css_class("toolbar");
        let count = gtk::Label::new(None);
        count.add_css_class("count");
        count.set_hexpand(true);
        count.set_halign(gtk::Align::End);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.set_transition_type(gtk::StackTransitionType::None);

        // Find bar
        let find_entry = gtk::SearchEntry::new();
        find_entry.set_hexpand(true);
        find_entry.set_placeholder_text(Some("Rechercher"));
        let find_label = gtk::Label::new(None);
        let find_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        find_box.add_css_class("findbar");
        let prev = gtk::Button::from_icon_name("go-up-symbolic");
        prev.set_tooltip_text(Some("Précédent (Maj+Entrée)"));
        let next = gtk::Button::from_icon_name("go-down-symbolic");
        next.set_tooltip_text(Some("Suivant (Entrée)"));
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        find_box.append(&find_entry);
        find_box.append(&find_label);
        find_box.append(&prev);
        find_box.append(&next);
        find_box.append(&close);
        let find_revealer = gtk::Revealer::builder().child(&find_box).reveal_child(false).build();

        if tiling_wm() {
            root.append(&bar_wrap);
        } else {
            // Floating desktops (GNOME, KDE…): the tab bar is the title bar,
            // draggable, with the usual window buttons.
            bar.append(&gtk::WindowControls::new(gtk::PackType::End));
            let handle = gtk::WindowHandle::new();
            handle.set_child(Some(&bar_wrap));
            win.set_titlebar(Some(&handle));
        }
        root.append(&toolbar);
        // Update notice (see update.rs).
        let update_label = gtk::Label::new(None);
        update_label.set_hexpand(true);
        update_label.set_xalign(0.0);
        update_label.set_wrap(true);
        let update_button = gtk::Button::with_label("Redémarrer");
        update_button.set_can_focus(false);
        let update_close = gtk::Button::from_icon_name("window-close-symbolic");
        update_close.set_can_focus(false);
        let update_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        update_box.add_css_class("updatebar");
        update_box.append(&update_label);
        update_box.append(&update_button);
        update_box.append(&update_close);
        let update_bar = gtk::Revealer::builder().child(&update_box).reveal_child(false).build();
        let ub = update_bar.clone();
        update_close.connect_clicked(move |_| ub.set_reveal_child(false));
        root.append(&update_bar);
        root.append(&stack);
        root.append(&find_revealer);
        win.set_child(Some(&root));

        let search_settings = sourceview5::SearchSettings::new();
        search_settings.set_wrap_around(true);
        search_settings.set_case_sensitive(false);

        let w = Rc::new(Window {
            win,
            tabs: RefCell::new(Vec::new()),
            headers: RefCell::new(Vec::new()),
            active: Cell::new(0),
            stack,
            tabbar,
            tab_scroller,
            toolbar,
            rich_buttons: RefCell::new(Vec::new()),
            source_button: RefCell::new(None),
            update_bar,
            update_label,
            update_button: RefCell::new(update_button),
            count,
            find_revealer,
            find_entry,
            find_label,
            find_hits: RefCell::new(Vec::new()),
            find_cur: Cell::new(None),
            find_origin: Cell::new((0, 0, 0, 0)),
            find_tab: RefCell::new(None),
            find_timer: RefCell::new(None),
            search_settings,
            session,
            theme,
            css,
            scheme,
            generation,
            detached,
            flush_timer: RefCell::new(None),
            changed: RefCell::new(HashSet::new()),
            monitors: RefCell::new(Vec::new()),
        });
        WINDOWS.with(|ws| ws.borrow_mut().push(w.clone()));
        if !detached {
            w.apply_theme();
        }
        w.toolbar.set_visible(w.session.borrow().toolbar);
        w.build_toolbar();
        w.connect_find(&prev, &next, &close);
        w.setup_shortcuts();
        w.setup_drop();

        let weak = Rc::downgrade(&w);
        new_btn.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.new_tab();
            }
        });
        // Double-click on the empty tab bar opens a tab.
        let dbl = gtk::GestureClick::new();
        let weak = Rc::downgrade(&w);
        dbl.connect_pressed(move |_, n, _, _| {
            if n == 2
                && let Some(w) = weak.upgrade()
            {
                w.new_tab();
            }
        });
        bar.add_controller(dbl);
        for (widget, positional) in [(bar.upcast_ref::<gtk::Widget>().clone(), true), (w.win.upcast_ref::<gtk::Widget>().clone(), false)] {
            let target = gtk::DropTarget::new(gtk::Box::static_type(), gdk::DragAction::MOVE);
            let weak = Rc::downgrade(&w);
            let wref = widget.clone();
            target.connect_drop(move |_, value, x, _| {
                let (Some(w), Ok(header)) = (weak.upgrade(), value.get::<gtk::Box>()) else { return false };
                let id = header.widget_name().to_string();
                if window_of_tab(&id).is_none() {
                    return false;
                }
                TAB_DROPPED.with(|d| d.set(true));
                let at = if positional {
                    Some(w.drop_index(&wref, x))
                } else if let Some(i) = w.tabs.borrow().iter().position(|t| t.id == id) {
                    // Pulled out of the tab bar onto its own window (a tiled
                    // window leaves no "outside"): detach, as browsers do.
                    let w2 = w.clone();
                    glib::idle_add_local_once(move || w2.detach_tab(i));
                    return true;
                } else {
                    None
                };
                let w2 = w.clone();
                glib::idle_add_local_once(move || w2.move_tab_here(&id, at));
                true
            });
            widget.add_controller(target);
        }

        let weak = Rc::downgrade(&w);
        w.win.connect_close_request(move |win| {
            let Some(w) = weak.upgrade() else { return glib::Propagation::Proceed };
            if w.detached {
                w.close_detached();
                return glib::Propagation::Stop;
            }
            w.flush();
            // Hide only: the process stays resident (Ctrl+Q quits).
            win.set_visible(false);
            glib::Propagation::Stop
        });

        if detached {
            return w;
        }
        let weak = Rc::downgrade(&w);
        *w.monitors.borrow_mut() = theme::watch(move |t| {
            if let Some(w) = weak.upgrade()
                && *w.theme.borrow() != t
            {
                *w.theme.borrow_mut() = t;
                w.apply_theme();
            }
        });

        w
    }

    // -----------------------------------------------------------------------
    // Theme
    // -----------------------------------------------------------------------

    fn apply_theme(&self) {
        let theme = self.theme.borrow().clone();
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let scheme = theme::install_scheme(&theme, generation);
        *self.scheme.borrow_mut() = scheme.clone();
        if let Some(settings) = gtk::Settings::default() {
            // GTK 4.20 replaced prefer-dark by gtk-interface-color-scheme.
            match settings.find_property("gtk-interface-color-scheme") {
                Some(pspec) => {
                    if let Some(class) = glib::EnumClass::with_type(pspec.value_type())
                        && let Some(v) = class.to_value_by_nick(if theme.dark { "dark" } else { "light" })
                    {
                        settings.set_property_from_value("gtk-interface-color-scheme", &v);
                    }
                }
                None => settings.set_gtk_application_prefer_dark_theme(theme.dark),
            }
        }
        self.css.load_from_string(&theme.css(self.session.borrow().font_size));
        for w in all_windows() {
            for t in w.tabs.borrow().iter() {
                t.apply_theme(&theme, scheme.as_ref());
            }
        }
    }

    fn zoom(self: &Rc<Self>, delta: f64) {
        {
            let mut s = self.session.borrow_mut();
            s.font_size = if delta == 0.0 { 12.0 } else { (s.font_size + delta).clamp(7.0, 32.0) };
        }
        let theme = self.theme.borrow().clone();
        self.css.load_from_string(&theme.css(self.session.borrow().font_size));
        for w in all_windows() {
            for t in w.tabs.borrow().iter() {
                t.schedule_table_sync();
            }
        }
        // Saved with the session: the size stays for every later use.
        self.schedule_flush();
    }

    // -----------------------------------------------------------------------
    // Tabs
    // -----------------------------------------------------------------------

    /// The notes window's title starts with "Calepin": the Hyprland rule
    /// that floats it on the notes workspace matches on that.
    fn title_for(&self, tab: &Tab) -> String {
        if self.detached { format!("{} — Calepin", tab.title()) } else { format!("Calepin — {}", tab.title()) }
    }

    pub fn active_tab(&self) -> Option<TabRef> {
        self.tabs.borrow().get(self.active.get()).cloned()
    }

    fn make_tab(self: &Rc<Self>, id: String) -> TabRef {
        let theme = self.theme.borrow().clone();
        let tab = Tab::new(id, &theme, self.scheme.borrow().as_ref(), &self.search_settings);
        tab.map.set_visible(self.session.borrow().minimap);
        self.bind_tab(&tab);
        let drop = file_drop_target();
        tab.view.add_controller(drop);
        tab
    }

    /// Routes a tab's events to this window (again, after a move).
    fn bind_tab(self: &Rc<Self>, tab: &TabRef) {
        let weak = Rc::downgrade(self);
        let id = tab.id.clone();
        let weak_zoom = Rc::downgrade(self);
        tab.set_on_zoom(Rc::new(move |d| {
            if let Some(w) = weak_zoom.upgrade() {
                w.zoom(d);
            }
        }));
        tab.set_on_change(Rc::new(move || {
            if let Some(w) = weak.upgrade() {
                w.changed.borrow_mut().insert(id.clone());
                w.schedule_flush();
                w.schedule_find_refresh();
            }
        }));
    }

    fn insert_tab(self: &Rc<Self>, tab: TabRef, at: Option<usize>) -> usize {
        let idx = at.unwrap_or(self.tabs.borrow().len()).min(self.tabs.borrow().len());
        self.stack.add_named(&tab.widget, Some(&tab.id));
        self.tabs.borrow_mut().insert(idx, tab);
        self.rebuild_tabbar();
        idx
    }

    pub fn new_tab(self: &Rc<Self>) {
        let tab = self.make_tab(session::new_id());
        tab.set_content("", true);
        let idx = self.insert_tab(tab, Some(self.active.get() + 1));
        self.activate(idx);
        self.save_session();
    }

    pub fn activate(&self, idx: usize) {
        let Some(tab) = self.tabs.borrow().get(idx).cloned() else { return };
        self.active.set(idx);
        self.stack.set_visible_child(&tab.widget);
        for (i, h) in self.headers.borrow().iter().enumerate() {
            if i == idx {
                h.root.add_css_class("active");
            } else {
                h.root.remove_css_class("active");
            }
        }
        if let Some(h) = self.headers.borrow().get(idx) {
            // Keep the active tab visible in the bar.
            let adj = self.tab_scroller.hadjustment();
            let root = h.root.clone();
            let bar = self.tabbar.clone();
            glib::idle_add_local_once(move || {
                if let Some(b) = root.compute_bounds(&bar) {
                    let (x, w) = (b.x() as f64, b.width() as f64);
                    if x < adj.value() {
                        adj.set_value(x);
                    } else if x + w > adj.value() + adj.page_size() {
                        adj.set_value(x + w - adj.page_size());
                    }
                }
            });
        }
        self.update_toolbar(&tab);
        self.update_count();
        self.win.set_title(Some(&self.title_for(&tab)));
        tab.view.grab_focus();
        if self.find_open() {
            self.set_find_origin();
            self.run_find(true);
        }
    }

    fn rebuild_tabbar(self: &Rc<Self>) {
        while let Some(c) = self.tabbar.first_child() {
            self.tabbar.remove(&c);
        }
        let mut headers = Vec::new();
        for tab in self.tabs.borrow().iter() {
            let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            root.add_css_class("tab");
            let label = gtk::Label::new(Some(&tab.title()));
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(24);
            let dirty = gtk::Label::new(Some("●"));
            dirty.add_css_class("dirty");
            let file = gtk::Image::from_icon_name("text-x-generic-symbolic");
            file.add_css_class("file");
            let close = gtk::Button::from_icon_name("window-close-symbolic");
            close.add_css_class("close");
            close.set_can_focus(false);
            close.set_tooltip_text(Some("Fermer (Ctrl+W)"));
            root.append(&file);
            root.append(&label);
            root.append(&dirty);
            root.append(&close);

            let id = tab.id.clone();
            let weak = Rc::downgrade(self);
            close.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade()
                    && let Some(i) = w.index_of(&id)
                {
                    w.request_close(i);
                }
            });
            let click = gtk::GestureClick::new();
            click.set_button(0);
            let id = tab.id.clone();
            let weak = Rc::downgrade(self);
            click.connect_pressed(move |g, _, _, _| {
                let Some(w) = weak.upgrade() else { return };
                let Some(i) = w.index_of(&id) else { return };
                match g.current_button() {
                    2 => w.request_close(i),
                    _ => w.activate(i),
                }
            });
            root.add_controller(click);
            // Drag a tab: onto a tab bar to move it, out of every Calepin
            // window to open it in a window of its own.
            root.set_widget_name(&tab.id);
            let drag = gtk::DragSource::new();
            drag.set_actions(gdk::DragAction::MOVE);
            let payload = root.clone();
            drag.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&payload.to_value())));
            let icon = root.clone();
            drag.connect_drag_begin(move |src, _| {
                TAB_DROPPED.with(|d| d.set(false));
                src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&icon))), 24, 12);
            });
            let id = tab.id.clone();
            drag.connect_drag_cancel(move |_, _, reason| {
                // Dropped where no window takes it. Wayland reports that as
                // an error, not as "no target"; Escape is a user cancel.
                if reason != gdk::DragCancelReason::UserCancelled && !TAB_DROPPED.with(|d| d.replace(false)) {
                    let id = id.clone();
                    glib::idle_add_local_once(move || {
                        if let Some((w, i)) = window_of_tab(&id) {
                            w.detach_tab(i);
                        }
                    });
                }
                false
            });
            root.add_controller(drag);
            self.tabbar.append(&root);
            let h = Header { root, label, file, dirty };
            show_state(&h, tab.path.borrow().as_deref(), self.is_dirty(tab));
            headers.push(h);
        }
        *self.headers.borrow_mut() = headers;
        let a = self.active.get();
        for (i, h) in self.headers.borrow().iter().enumerate() {
            if i == a {
                h.root.add_css_class("active");
            }
        }
    }

    fn refresh_headers(&self) {
        let tabs = self.tabs.borrow();
        for (tab, h) in tabs.iter().zip(self.headers.borrow().iter()) {
            let t = tab.title();
            if h.label.text() != t {
                h.label.set_text(&t);
            }
            show_state(h, tab.path.borrow().as_deref(), self.is_dirty(tab));
        }
        if let Some(tab) = tabs.get(self.active.get()) {
            self.win.set_title(Some(&self.title_for(tab)));
        }
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        self.tabs.borrow().iter().position(|t| t.id == id)
    }

    fn is_dirty(&self, tab: &Tab) -> bool {
        tab.path.borrow().is_some() && tab.saved.borrow().as_deref() != Some(tab.content().as_str())
    }

    fn tab_state(&self, tab: &Tab) -> TabState {
        TabState {
            id: tab.id.clone(),
            path: tab.path.borrow().clone(),
            markdown: tab.markdown.get(),
            dirty: self.is_dirty(tab),
            cursor: tab.cursor_offset(),
            wide_tables: tab.wide_tables(),
        }
    }

    pub fn close_tab(self: &Rc<Self>, idx: usize) {
        let Some(tab) = self.tabs.borrow().get(idx).cloned() else { return };
        self.retire(&tab);
        self.changed.borrow_mut().remove(&tab.id);
        self.stack.remove(&tab.widget);
        self.tabs.borrow_mut().remove(idx);
        self.after_removal(idx);
    }

    /// A closed tab's content stays recoverable (Ctrl+Shift+T).
    fn retire(&self, tab: &Tab) {
        let content = tab.content();
        if content.trim().is_empty() && tab.path.borrow().is_none() {
            session::delete_content(&tab.id);
        } else {
            session::save_content(&tab.id, &content);
            let state = self.tab_state(tab);
            let mut s = self.session.borrow_mut();
            s.closed.retain(|c| c.id != state.id);
            s.closed.push(state);
            while s.closed.len() > session::MAX_CLOSED {
                let old = s.closed.remove(0);
                session::delete_content(&old.id);
            }
        }
    }

    /// After a tab left this window: show a neighbour, or close an empty
    /// detached window (the notes window always keeps a tab).
    fn after_removal(self: &Rc<Self>, idx: usize) {
        if self.tabs.borrow().is_empty() {
            if self.detached {
                let me = self.clone();
                glib::idle_add_local_once(move || me.close_window());
                return;
            }
            self.rebuild_tabbar();
            self.new_tab();
            return;
        }
        let a = self.active.get();
        let next = if a > idx || a >= self.tabs.borrow().len() { a.saturating_sub(1) } else { a };
        self.rebuild_tabbar();
        self.activate(next.min(self.tabs.borrow().len() - 1));
        self.save_session();
    }

    /// Removes a tab from this window without closing it.
    fn take_tab(self: &Rc<Self>, idx: usize) -> Option<TabRef> {
        let tab = self.tabs.borrow().get(idx).cloned()?;
        session::save_content(&tab.id, &tab.content());
        self.changed.borrow_mut().remove(&tab.id);
        if self.find_tab.borrow().as_deref() == Some(tab.id.as_str()) {
            self.clear_find_marks();
        }
        self.stack.remove(&tab.widget);
        self.tabs.borrow_mut().remove(idx);
        Some(tab)
    }

    fn adopt(self: &Rc<Self>, tab: TabRef, at: Option<usize>) {
        self.bind_tab(&tab);
        tab.map.set_visible(self.session.borrow().minimap);
        self.watch_file(&tab);
        let idx = self.insert_tab(tab, at);
        self.activate(idx);
        self.save_session();
    }

    /// Moves a tab (from any window) into this one, at `at`.
    pub fn move_tab_here(self: &Rc<Self>, id: &str, at: Option<usize>) {
        let Some((src, i)) = window_of_tab(id) else { return };
        if Rc::ptr_eq(&src, self) {
            let n = self.tabs.borrow().len();
            let mut to = at.unwrap_or(n).min(n);
            if to > i {
                to -= 1;
            }
            if to == i {
                return;
            }
            let tab = self.tabs.borrow_mut().remove(i);
            self.tabs.borrow_mut().insert(to, tab);
            self.rebuild_tabbar();
            self.activate(to);
            self.save_session();
            return;
        }
        let Some(tab) = src.take_tab(i) else { return };
        src.after_removal(i);
        self.adopt(tab, at);
        self.win.present();
    }

    /// Opens the tab in a new window on the current workspace.
    pub fn detach_tab(self: &Rc<Self>, idx: usize) {
        if self.detached && self.tabs.borrow().len() <= 1 {
            return;
        }
        let Some(id) = self.tabs.borrow().get(idx).map(|t| t.id.clone()) else { return };
        // The notes workspace would otherwise receive the new window.
        leave_special_workspace();
        let w = Window::new_detached(self);
        w.move_tab_here(&id, None);
        w.win.present();
    }

    /// Closing a detached window: its tabs close like Ctrl+W, after a
    /// question when some are not saved to a file.
    fn close_detached(self: &Rc<Self>) {
        self.flush();
        let unsaved = self.tabs.borrow().iter().filter(|t| self.needs_confirm(t)).count();
        if unsaved == 0 {
            self.discard_all();
            return;
        }
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(if unsaved == 1 { "Un onglet n'est pas enregistré dans un fichier".to_string() } else { format!("{unsaved} onglets ne sont pas enregistrés dans un fichier") })
            .detail("Remettre dans les notes garde tous les onglets dans la fenêtre de SUPER+N. Fermés, ils restent récupérables avec Ctrl+Maj+T.")
            .buttons(["Annuler", "Fermer sans enregistrer", "Remettre dans les notes"])
            .cancel_button(0)
            .default_button(2)
            .build();
        let w = self.clone();
        glib::spawn_future_local(async move {
            match dialog.choose_future(Some(&w.win)).await.unwrap_or(0) {
                1 => w.discard_all(),
                2 => {
                    if let Some(main) = main_window() {
                        let ids: Vec<String> = w.tabs.borrow().iter().map(|t| t.id.clone()).collect();
                        for id in ids {
                            main.move_tab_here(&id, None);
                        }
                    }
                }
                _ => {}
            }
        });
    }

    fn discard_all(self: &Rc<Self>) {
        let tabs: Vec<TabRef> = self.tabs.borrow_mut().drain(..).collect();
        for t in &tabs {
            self.retire(t);
            self.stack.remove(&t.widget);
        }
        self.changed.borrow_mut().clear();
        self.close_window();
    }

    /// Forgets a detached window and destroys it. (GTK's "destroy" only
    /// comes once nothing holds the window: the registry does.)
    fn close_window(self: &Rc<Self>) {
        WINDOWS.with(|ws| ws.borrow_mut().retain(|x| !Rc::ptr_eq(x, self)));
        if let Some(m) = main_window() {
            m.save_session();
        }
        self.win.destroy();
    }

    /// Index in the tab bar for a drop at `x` (tab bar coordinates).
    fn drop_index(&self, bar: &gtk::Widget, x: f64) -> usize {
        let headers = self.headers.borrow();
        for (i, h) in headers.iter().enumerate() {
            if let Some(b) = h.root.compute_bounds(bar)
                && x < (b.x() + b.width() / 2.0) as f64
            {
                return i;
            }
        }
        headers.len()
    }

    pub fn reopen_closed(self: &Rc<Self>) {
        let Some(state) = self.session.borrow_mut().closed.pop() else { return };
        if let Some(i) = self.tabs.borrow().iter().position(|t| t.id == state.id) {
            self.activate(i);
            return;
        }
        let tab = self.tab_from_state(&state);
        let idx = self.insert_tab(tab, Some(self.active.get() + 1));
        self.activate(idx);
        self.save_session();
    }

    fn tab_from_state(self: &Rc<Self>, state: &TabState) -> TabRef {
        let tab = self.make_tab(state.id.clone());
        *tab.path.borrow_mut() = state.path.clone();
        let stored = session::load_content(&state.id);
        let decoded = state.path.as_ref().and_then(|p| std::fs::read(p).ok()).map(|b| decode(&b));
        let readonly = decoded.as_ref().is_some_and(|d| d.1);
        let disk = decoded.map(|d| d.0);
        let text = match (&disk, &stored) {
            (Some(_), Some(s)) if state.dirty => s.clone(),
            (Some(d), _) => d.clone(),
            (None, Some(s)) => s.clone(),
            (None, None) => String::new(),
        };
        tab.set_content(&text, state.markdown && !readonly);
        tab.set_readonly(readonly);
        if let Some(d) = disk {
            tab.set_disk(d);
        }
        tab.set_wide_tables(&state.wide_tables);
        tab.restore_cursor(state.cursor);
        self.watch_file(&tab);
        tab
    }

    fn restore(self: &Rc<Self>) {
        let states: Vec<TabState> = {
            let mut s = self.session.borrow_mut();
            let detached: Vec<TabState> = s.detached.drain(..).flatten().collect();
            s.tabs.extend(detached);
            s.tabs.clone()
        };
        for st in &states {
            let tab = self.tab_from_state(st);
            self.insert_tab(tab, None);
        }
        if self.tabs.borrow().is_empty() {
            let tab = self.make_tab(session::new_id());
            tab.set_content("", true);
            self.insert_tab(tab, None);
        }
        let a = self.session.borrow().active.min(self.tabs.borrow().len() - 1);
        self.activate(a);
        self.toolbar.set_visible(self.session.borrow().toolbar);
        self.refresh_headers();
        session::gc(&self.session.borrow());
    }

    fn move_tab(self: &Rc<Self>, delta: isize) {
        let a = self.active.get() as isize;
        let n = self.tabs.borrow().len() as isize;
        let to = a + delta;
        if to < 0 || to >= n {
            return;
        }
        self.tabs.borrow_mut().swap(a as usize, to as usize);
        self.active.set(to as usize);
        self.rebuild_tabbar();
        self.activate(to as usize);
        self.save_session();
    }

    fn cycle(&self, delta: isize) {
        let n = self.tabs.borrow().len() as isize;
        if n == 0 {
            return;
        }
        let i = (self.active.get() as isize + delta).rem_euclid(n);
        self.activate(i as usize);
    }

    // -----------------------------------------------------------------------
    // Files
    // -----------------------------------------------------------------------

    /// Opens a file in a tab of this window, unless a window already shows
    /// it. Returns the window that does.
    pub fn open_path(self: &Rc<Self>, path: &Path) -> WindowRef {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        for w in all_windows() {
            let found = w.tabs.borrow().iter().position(|t| t.path.borrow().as_deref() == Some(path.as_path()));
            if let Some(i) = found {
                w.activate(i);
                return w;
            }
        }
        let (text, readonly) = match std::fs::read(&path) {
            Ok(b) => decode(&b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), false),
            Err(e) => {
                eprintln!("calepin: cannot open {}: {e}", path.display());
                return self.clone();
            }
        };
        // Reuse an untouched empty note.
        let reuse = self.active_tab().filter(|t| t.path.borrow().is_none() && t.is_empty());
        let tab = match &reuse {
            Some(t) => t.clone(),
            None => self.make_tab(session::new_id()),
        };
        *tab.path.borrow_mut() = Some(path.clone());
        tab.set_content(&text, is_markdown_path(&path) && !readonly);
        tab.set_readonly(readonly);
        tab.set_disk(text);
        self.watch_file(&tab);
        if reuse.is_some() {
            self.rebuild_tabbar();
            self.activate(self.active.get());
        } else {
            let idx = self.insert_tab(tab, Some(self.active.get() + 1));
            self.activate(idx);
        }
        self.refresh_headers();
        self.save_session();
        self.clone()
    }

    fn watch_file(self: &Rc<Self>, tab: &TabRef) {
        let Some(path) = tab.path.borrow().clone() else { return };
        let Ok(monitor) = gio::File::for_path(&path).monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) else {
            return;
        };
        let weak_tab = Rc::downgrade(tab);
        let weak = Rc::downgrade(self);
        monitor.connect_changed(move |_, _, _, ev| {
            if !matches!(ev, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created | gio::FileMonitorEvent::MovedIn | gio::FileMonitorEvent::Renamed) {
                return;
            }
            let (Some(tab), Some(w)) = (weak_tab.upgrade(), weak.upgrade()) else { return };
            let Some(path) = tab.path.borrow().clone() else { return };
            let Ok(bytes) = std::fs::read(&path) else { return };
            let (disk, _) = decode(&bytes);
            if tab.disk.borrow().as_deref() == Some(disk.as_str()) {
                return;
            }
            if w.is_dirty(&tab) {
                // Local edits win; the file shows as modified.
                tab.set_disk(disk);
            } else {
                let cur = tab.cursor_offset();
                tab.set_content(&disk, tab.markdown.get());
                tab.set_disk(disk);
                tab.restore_cursor(cur);
            }
            w.refresh_headers();
        });
        *tab.monitor.borrow_mut() = Some(monitor);
    }

    fn write_file(self: &Rc<Self>, tab: &TabRef, path: PathBuf) -> bool {
        if tab.readonly.get() {
            self.error("Ce fichier n'est pas du texte UTF-8 : il est ouvert en lecture seule.");
            return false;
        }
        let content = tab.content();
        match std::fs::write(&path, content.as_bytes()) {
            Ok(()) => {
                let path = std::fs::canonicalize(&path).unwrap_or(path);
                let was_new = tab.path.borrow().as_deref() != Some(path.as_path());
                *tab.saved.borrow_mut() = Some(content.clone());
                *tab.disk.borrow_mut() = Some(content);
                *tab.path.borrow_mut() = Some(path);
                if was_new {
                    self.watch_file(tab);
                    self.rebuild_tabbar();
                }
                self.refresh_headers();
                self.save_session();
                true
            }
            Err(e) => {
                self.error(&format!("Impossible d'enregistrer : {e}"));
                false
            }
        }
    }

    fn error(&self, msg: &str) {
        let d = gtk::AlertDialog::builder().message(msg).build();
        d.show(Some(&self.win));
    }

    pub fn save(self: &Rc<Self>, save_as: bool) {
        let Some(tab) = self.active_tab() else { return };
        let w = self.clone();
        glib::spawn_future_local(async move {
            w.save_tab(&tab, save_as).await;
        });
    }

    /// Saves `tab`, asking for a path when it has none. True once written.
    async fn save_tab(self: &Rc<Self>, tab: &TabRef, save_as: bool) -> bool {
        let path = tab.path.borrow().clone();
        if let Some(p) = &path
            && !save_as
        {
            return self.write_file(tab, p.clone());
        }
        let dialog = gtk::FileDialog::new();
        let name = match &path {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            None => {
                let t: String = tab.title().chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '…')).collect();
                format!("{}.md", if t == "Nouvelle note" { "note".into() } else { t.trim().to_string() })
            }
        };
        dialog.set_initial_name(Some(&name));
        if let Some(dir) = path.as_ref().and_then(|p| p.parent()) {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        } else if let Some(docs) = glib::user_special_dir(glib::UserDirectory::Documents) {
            dialog.set_initial_folder(Some(&gio::File::for_path(docs)));
        }
        match dialog.save_future(Some(&self.win)).await {
            Ok(file) => match file.path() {
                Some(p) => self.write_file(tab, p),
                None => false,
            },
            Err(_) => false,
        }
    }

    fn needs_confirm(&self, tab: &Tab) -> bool {
        if tab.readonly.get() {
            return false;
        }
        match tab.path.borrow().as_ref() {
            None => !tab.content().trim().is_empty(),
            Some(_) => self.is_dirty(tab),
        }
    }

    /// Closes a tab, asking first when its content is not saved to a file.
    pub fn request_close(self: &Rc<Self>, idx: usize) {
        let Some(tab) = self.tabs.borrow().get(idx).cloned() else { return };
        if !self.needs_confirm(&tab) {
            self.close_tab(idx);
            return;
        }
        self.activate(idx);
        let never = tab.path.borrow().is_none();
        let detail = if never {
            "Cette note n'a jamais été enregistrée dans un fichier. Fermée sans enregistrer, elle reste récupérable avec Ctrl+Maj+T."
        } else {
            "Les modifications ne sont pas enregistrées dans le fichier. Fermé sans enregistrer, l'onglet reste récupérable avec Ctrl+Maj+T."
        };
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(format!("Enregistrer « {} » avant de fermer ?", tab.title()))
            .detail(detail)
            .buttons(["Annuler", "Fermer sans enregistrer", if never { "Enregistrer…" } else { "Enregistrer" }])
            .cancel_button(0)
            .default_button(2)
            .build();
        let w = self.clone();
        glib::spawn_future_local(async move {
            let choice = dialog.choose_future(Some(&w.win)).await.unwrap_or(0);
            let close = match choice {
                1 => true,
                2 => w.save_tab(&tab, false).await,
                _ => false,
            };
            if close && let Some(i) = w.index_of(&tab.id) {
                w.close_tab(i);
            } else {
                tab.view.grab_focus();
            }
        });
    }

    pub fn open_dialog(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::new();
        if let Some(p) = self.active_tab().and_then(|t| t.path.borrow().clone())
            && let Some(dir) = p.parent()
        {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let w = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(files) = dialog.open_multiple_future(Some(&w.win)).await {
                for i in 0..files.n_items() {
                    if let Some(f) = files.item(i).and_downcast::<gio::File>()
                        && let Some(p) = f.path()
                    {
                        w.open_path(&p);
                    }
                }
            }
        });
    }

    fn setup_drop(self: &Rc<Self>) {
        self.win.add_controller(file_drop_target());
    }
}

/// Files dropped on a window open in that window.
fn file_drop_target() -> gtk::DropTarget {
    {
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.set_propagation_phase(gtk::PropagationPhase::Capture);
        drop.connect_drop(move |d, value, _, _| {
            let root = d.widget().and_then(|w| w.root());
            let w = all_windows().into_iter().find(|w| root.as_ref().is_some_and(|r| r.upcast_ref::<gtk::Widget>() == w.win.upcast_ref::<gtk::Widget>()));
            let (Some(w), Ok(list)) = (w, value.get::<gdk::FileList>()) else { return false };
            for f in list.files() {
                if let Some(p) = f.path() {
                    w.open_path(&p);
                }
            }
            true
        });
        drop
    }
}

impl Window {

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    fn schedule_flush(self: &Rc<Self>) {
        if self.flush_timer.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(350), move || {
            if let Some(w) = weak.upgrade() {
                w.flush_timer.borrow_mut().take();
                w.flush();
            }
        });
        *self.flush_timer.borrow_mut() = Some(id);
    }

    /// Writes every changed tab and the session. Cheap; called often.
    pub fn flush(&self) {
        if let Some(id) = self.flush_timer.borrow_mut().take() {
            id.remove();
        }
        for w in all_windows() {
            let changed: Vec<String> = w.changed.borrow_mut().drain().collect();
            for tab in w.tabs.borrow().iter() {
                if changed.contains(&tab.id) {
                    session::save_content(&tab.id, &tab.content());
                }
            }
        }
        self.save_session();
        self.refresh_headers();
        self.update_count();
    }

    /// The session of every window: the notes window's tabs, and the
    /// detached ones (back in the notes at the next start).
    fn save_session(&self) {
        let wins = all_windows();
        let main = wins.iter().find(|w| !w.detached);
        let tabs = main.map(|m| m.tabs.borrow().iter().map(|t| m.tab_state(t)).collect::<Vec<_>>());
        let active = main.map(|m| m.active.get());
        let detached: Vec<Vec<TabState>> =
            wins.iter().filter(|w| w.detached).map(|w| w.tabs.borrow().iter().map(|t| w.tab_state(t)).collect()).collect();
        let mut s = self.session.borrow_mut();
        if let (Some(tabs), Some(active)) = (tabs, active) {
            s.tabs = tabs;
            s.active = active;
        }
        s.detached = detached;
        session::save(&s);
    }

    fn update_count(&self) {
        if let Some(t) = self.active_tab() {
            let n = t.word_count();
            self.count.set_text(&format!("{n} mot{}", if n > 1 { "s" } else { "" }));
        }
    }

    // -----------------------------------------------------------------------
    // Toolbar and commands
    // -----------------------------------------------------------------------

    fn build_toolbar(self: &Rc<Self>) {
        let mut rich: Vec<gtk::Widget> = Vec::new();
        type Cmd = fn(&Rc<Window>);
        let items: Vec<Option<(&str, &str, bool, Cmd)>> = vec![
            Some(("Texte", "Texte normal (Ctrl+Alt+0)", false, |w| w.block(BlockCmd::Para))),
            Some(("H1", "Titre 1 (Ctrl+Alt+1)", false, |w| w.block(BlockCmd::Heading(1)))),
            Some(("H2", "Titre 2 (Ctrl+Alt+2)", false, |w| w.block(BlockCmd::Heading(2)))),
            Some(("H3", "Titre 3 (Ctrl+Alt+3)", false, |w| w.block(BlockCmd::Heading(3)))),
            None,
            Some(("<b>B</b>", "Gras (Ctrl+B)", true, |w| w.inline("b"))),
            Some(("<i>I</i>", "Italique (Ctrl+I)", true, |w| w.inline("i"))),
            Some(("<s>S</s>", "Barré (Ctrl+Maj+X)", true, |w| w.inline("s"))),
            Some(("&lt;/&gt;", "Code (Ctrl+E)", true, |w| w.inline("code"))),
            Some(("Lien", "Lien (Ctrl+K)", false, |w| w.link_popover())),
            None,
            Some(("•", "Liste à puces (Ctrl+Alt+5)", false, |w| w.block(BlockCmd::Bullet))),
            Some(("1.", "Liste numérotée (Ctrl+Alt+6)", false, |w| w.block(BlockCmd::Numbered))),
            Some(("☐", "Case à cocher (Ctrl+Alt+4)", false, |w| w.block(BlockCmd::Check))),
            Some(("❝", "Citation (Ctrl+Alt+Q)", false, |w| w.block(BlockCmd::Quote))),
            Some(("{ }", "Bloc de code (Ctrl+Alt+8)", false, |w| w.block(BlockCmd::Code))),
            Some(("▦", "Tableau (Ctrl+Alt+T)", false, |w| w.table())),
            None,
            Some(("T̶x", "Effacer la mise en forme (Ctrl+\\)", false, |w| w.clear())),
        ];
        for item in items {
            match item {
                None => self.toolbar.append(&sep()),
                Some((label, tip, markup, cmd)) => {
                    let b = btn(label, tip, markup);
                    let weak = Rc::downgrade(self);
                    b.connect_clicked(move |_| {
                        if let Some(w) = weak.upgrade() {
                            cmd(&w);
                        }
                    });
                    self.toolbar.append(&b);
                    rich.push(b.upcast());
                }
            }
        }
        self.toolbar.append(&self.count);
        let src = btn("MD", "Mode source Markdown (Ctrl+Maj+E)", false);
        let weak = Rc::downgrade(self);
        src.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.toggle_source();
            }
        });
        self.toolbar.append(&src);
        *self.source_button.borrow_mut() = Some(src);
        *self.rich_buttons.borrow_mut() = rich;
    }

    fn block(&self, cmd: BlockCmd) {
        if let Some(t) = self.active_tab() {
            t.set_block(cmd);
            t.view.grab_focus();
        }
    }

    pub fn focus_text(&self) {
        if let Some(t) = self.active_tab()
            && !self.find_revealer.reveals_child()
        {
            t.view.grab_focus();
        }
    }

    fn table(&self) {
        if let Some(t) = self.active_tab() {
            t.insert_table();
        }
    }

    fn inline(&self, name: &str) {
        if let Some(t) = self.active_tab() {
            t.toggle_inline(name);
            // Stay in a table cell when the command came from there.
            let in_cell = gtk::prelude::GtkWindowExt::focus(&self.win).is_some_and(|f| f.has_css_class("mw-cell"));
            if !in_cell {
                t.view.grab_focus();
            }
        }
    }

    fn clear(&self) {
        if let Some(t) = self.active_tab() {
            t.clear_format();
            t.view.grab_focus();
        }
    }

    /// Shows what the start-up update check found.
    pub fn show_update(self: &Rc<Self>, outcome: crate::update::Outcome) {
        use crate::update::Outcome;
        let (text, button, action, page): (String, &str, u8, String) = match outcome {
            Outcome::UpToDate => return,
            Outcome::Installed(r) => (format!("Calepin {} est installé. Redémarrer pour l'utiliser (les onglets sont conservés).", r.version), "Redémarrer", 1, r.page),
            Outcome::Available(r) => (format!("Calepin {} est disponible.", r.version), "Voir la version", 2, r.page),
            Outcome::Failed(r, e) => (format!("Calepin {} est disponible, mais la mise à jour a échoué : {e}", r.version), "Voir la version", 2, r.page),
        };
        self.update_label.set_text(&text);
        let weak = Rc::downgrade(self);
        // A fresh button: one click handler at a time.
        let b = self.update_button.borrow().clone();
        let new = gtk::Button::with_label(button);
        new.set_can_focus(false);
        if let Some(parent) = b.parent().and_downcast::<gtk::Box>() {
            parent.insert_child_after(&new, Some(&self.update_label));
            parent.remove(&b);
        }
        *self.update_button.borrow_mut() = new.clone();
        new.connect_clicked(move |_| {
            let Some(w) = weak.upgrade() else { return };
            if action == 1 {
                w.flush();
                let err = crate::update::restart();
                w.error(&format!("Redémarrage impossible : {err}"));
            } else {
                crate::editor::launch_uri(&page);
            }
        });
        self.update_bar.set_reveal_child(true);
    }

    /// Formatting needs the rich view; the source switch exists only for
    /// Markdown (notes and .md files), and names what it switches to.
    fn update_toolbar(&self, tab: &Tab) {
        let rich = tab.markdown.get();
        for b in self.rich_buttons.borrow().iter() {
            b.set_sensitive(rich);
        }
        if let Some(b) = self.source_button.borrow().as_ref() {
            let md = tab.path.borrow().as_ref().is_none_or(|p| is_markdown_path(p)) && !tab.readonly.get();
            b.set_visible(md);
            if let Some(l) = b.child().and_downcast::<gtk::Label>() {
                l.set_text(if rich { "MD" } else { "Rendu" });
            }
            b.set_tooltip_text(Some(if rich {
                "Afficher le Markdown brut (Ctrl+Maj+E)"
            } else {
                "Revenir au rendu (Ctrl+Maj+E)"
            }));
        }
    }

    fn toggle_source(&self) {
        if let Some(t) = self.active_tab() {
            if t.path.borrow().as_ref().is_some_and(|p| !is_markdown_path(p)) && !t.markdown.get() {
                return; // Not a Markdown file: stay in plain mode.
            }
            t.toggle_source();
            self.update_toolbar(&t);
            t.view.grab_focus();
        }
    }

    fn link_popover(self: &Rc<Self>) {
        let Some(tab) = self.active_tab() else { return };
        if !tab.markdown.get() {
            return;
        }
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("https://… (vide pour retirer)"));
        entry.set_width_chars(36);
        if let Some(u) = tab.current_link() {
            entry.set_text(&u);
        }
        let pop = gtk::Popover::new();
        pop.set_child(Some(&entry));
        pop.set_parent(&tab.view);
        let cursor = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
        let r = tab.view.iter_location(&cursor);
        let (x, y) = tab.view.buffer_to_window_coords(gtk::TextWindowType::Widget, r.x(), r.y());
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x, y, 1, r.height())));
        let t = tab.clone();
        let p = pop.clone();
        entry.connect_activate(move |e| {
            t.set_link(&e.text());
            p.popdown();
        });
        let t = tab.clone();
        pop.connect_closed(move |p| {
            let p = p.clone();
            let t = t.clone();
            glib::idle_add_local_once(move || {
                p.unparent();
                t.view.grab_focus();
            });
        });
        pop.popup();
        entry.grab_focus();
    }

    fn toggle_minimap(&self) {
        let v = {
            let mut s = self.session.borrow_mut();
            s.minimap = !s.minimap;
            s.minimap
        };
        for t in self.tabs.borrow().iter() {
            t.map.set_visible(v);
        }
        self.save_session();
    }

    fn toggle_toolbar(&self) {
        let v = {
            let mut s = self.session.borrow_mut();
            s.toolbar = !s.toolbar;
            s.toolbar
        };
        self.toolbar.set_visible(v);
        self.save_session();
    }

    // -----------------------------------------------------------------------
    // Find
    // -----------------------------------------------------------------------

    fn connect_find(self: &Rc<Self>, prev: &gtk::Button, next: &gtk::Button, close: &gtk::Button) {
        // Results follow every keystroke.
        self.find_entry.set_search_delay(0);
        let weak = Rc::downgrade(self);
        self.find_entry.connect_search_changed(move |_| {
            if let Some(w) = weak.upgrade() {
                w.run_find(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.find_entry.connect_activate(move |_| {
            if let Some(w) = weak.upgrade() {
                w.find_step(1);
            }
        });
        let weak = Rc::downgrade(self);
        self.find_entry.connect_stop_search(move |_| {
            if let Some(w) = weak.upgrade() {
                w.close_find();
            }
        });
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        key.connect_key_pressed(move |_, k, _, state| {
            if matches!(k, gdk::Key::Return | gdk::Key::KP_Enter)
                && state.contains(gdk::ModifierType::SHIFT_MASK)
                && let Some(w) = weak.upgrade()
            {
                w.find_step(-1);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.find_entry.add_controller(key);
        for (button, d) in [(prev, -1), (next, 1)] {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    w.find_step(d);
                }
            });
        }
        let weak = Rc::downgrade(self);
        close.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.close_find();
            }
        });
    }

    fn find_open(&self) -> bool {
        self.find_revealer.reveals_child()
    }

    /// Search starts from the cursor.
    fn set_find_origin(&self) {
        if let Some(t) = self.active_tab() {
            let off = t.buffer.iter_at_mark(&t.buffer.get_insert()).offset();
            self.find_origin.set((off, 0, 0, 0));
        }
    }

    fn open_find(&self) {
        self.set_find_origin();
        let was_open = self.find_open();
        self.find_revealer.set_reveal_child(true);
        if let Some(t) = self.active_tab()
            && let Some((s, e)) = t.buffer.selection_bounds()
            && s.line() == e.line()
        {
            // Setting the text runs the search.
            self.find_entry.set_text(&t.buffer.text(&s, &e, false));
        } else if !was_open {
            self.run_find(true);
        }
        self.find_entry.grab_focus();
        self.find_entry.select_region(0, -1);
    }

    fn close_find(&self) {
        self.find_revealer.set_reveal_child(false);
        let hit = self.find_cur.get().and_then(|i| self.find_hits.borrow().get(i).cloned());
        self.clear_find_marks();
        self.find_label.set_text("");
        if let Some(t) = self.active_tab() {
            match hit {
                Some(h) => t.select_hit(&h),
                None => {
                    t.view.grab_focus();
                }
            }
        }
    }

    fn clear_find_marks(&self) {
        let id = self.find_tab.borrow_mut().take();
        if let Some(t) = id.and_then(|id| self.tabs.borrow().iter().find(|t| t.id == id).cloned()) {
            t.clear_hits();
        }
        self.find_hits.borrow_mut().clear();
        self.find_cur.set(None);
    }

    /// Searches the active tab. `from_origin`: the current hit is the first
    /// one after where the search started; otherwise (text edited) the one
    /// nearest the previous current hit.
    fn run_find(&self, from_origin: bool) {
        let Some(t) = self.active_tab() else { return };
        if self.find_tab.borrow().as_deref() != Some(t.id.as_str()) {
            self.clear_find_marks();
        }
        let needle = self.find_entry.text().to_string();
        if needle.is_empty() {
            t.clear_hits();
            self.find_hits.borrow_mut().clear();
            self.find_cur.set(None);
            self.find_label.set_text("");
            return;
        }
        let hits = t.find(&needle);
        let anchor = if from_origin {
            self.find_origin.get()
        } else {
            self.find_cur.get().and_then(|i| self.find_hits.borrow().get(i).map(|h| h.key)).unwrap_or(self.find_origin.get())
        };
        let cur = if hits.is_empty() { None } else { Some(hits.iter().position(|h| h.key >= anchor).unwrap_or(0)) };
        *self.find_tab.borrow_mut() = Some(t.id.clone());
        *self.find_hits.borrow_mut() = hits;
        self.find_cur.set(cur);
        self.show_find(from_origin);
    }

    fn show_find(&self, reveal: bool) {
        let Some(t) = self.active_tab() else { return };
        let hits = self.find_hits.borrow();
        let cur = self.find_cur.get();
        t.show_hits(&hits, cur);
        if reveal && let Some(h) = cur.and_then(|i| hits.get(i)) {
            t.reveal_hit(h);
        }
        let n = hits.len();
        self.find_label.set_text(&match cur {
            _ if n == 0 => "Aucun résultat".to_string(),
            Some(i) => format!("{}/{n}", i + 1),
            None => format!("{n}"),
        });
        if n == 0 {
            self.find_entry.add_css_class("error");
        } else {
            self.find_entry.remove_css_class("error");
        }
    }

    fn find_step(&self, d: isize) {
        if !self.find_open() {
            self.open_find();
            return;
        }
        let n = self.find_hits.borrow().len();
        if n == 0 {
            return;
        }
        let i = match self.find_cur.get() {
            Some(i) => (i as isize + d).rem_euclid(n as isize) as usize,
            None => 0,
        };
        self.find_cur.set(Some(i));
        if let Some(h) = self.find_hits.borrow().get(i) {
            self.find_origin.set(h.key);
        }
        self.show_find(true);
    }

    /// The text changed under an open search: refresh, keeping the place.
    fn schedule_find_refresh(self: &Rc<Self>) {
        if !self.find_open() || self.find_timer.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(150), move || {
            if let Some(w) = weak.upgrade() {
                w.find_timer.borrow_mut().take();
                if w.find_open() {
                    w.run_find(false);
                }
            }
        });
        *self.find_timer.borrow_mut() = Some(id);
    }

    // -----------------------------------------------------------------------
    // Shortcuts
    // -----------------------------------------------------------------------

    fn setup_shortcuts(self: &Rc<Self>) {
        let ctrl = gtk::ShortcutController::new();
        ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
        ctrl.set_scope(gtk::ShortcutScope::Local);
        let add = |accels: &[&str], f: Command| {
            for a in accels {
                let Some(trigger) = gtk::ShortcutTrigger::parse_string(a) else {
                    eprintln!("calepin: bad accel {a}");
                    continue;
                };
                let weak = Rc::downgrade(self);
                let f = f.clone();
                let action = gtk::CallbackAction::new(move |_, _| {
                    if let Some(w) = weak.upgrade() {
                        f(&w);
                    }
                    glib::Propagation::Stop
                });
                ctrl.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
            }
        };
        add(&["<Control>t", "<Control>n"], Rc::new(|w| w.new_tab()));
        add(&["<Control>w"], Rc::new(|w| w.request_close(w.active.get())));
        add(&["<Control><Shift>d"], Rc::new(|w| w.detach_tab(w.active.get())));
        add(&["<Control><Shift>t"], Rc::new(|w| w.reopen_closed()));
        add(&["<Control>Tab", "<Control>Page_Down"], Rc::new(|w| w.cycle(1)));
        add(&["<Control><Shift>Tab", "<Control><Shift>ISO_Left_Tab", "<Control>Page_Up"], Rc::new(|w| w.cycle(-1)));
        add(&["<Control><Shift>Page_Down"], Rc::new(|w| w.move_tab(1)));
        add(&["<Control><Shift>Page_Up"], Rc::new(|w| w.move_tab(-1)));
        for i in 1..=9usize {
            add(&[&format!("<Alt>{i}")], Rc::new(move |w| {
                let n = w.tabs.borrow().len();
                w.activate(if i == 9 { n - 1 } else { (i - 1).min(n - 1) });
            }));
        }
        add(&["<Control>o"], Rc::new(|w| w.open_dialog()));
        add(&["<Control>s"], Rc::new(|w| w.save(false)));
        add(&["<Control><Shift>s"], Rc::new(|w| w.save(true)));
        add(&["<Control>f"], Rc::new(|w| w.open_find()));
        add(&["<Control>g", "F3"], Rc::new(|w| w.find_step(1)));
        add(&["<Control><Shift>g", "<Shift>F3"], Rc::new(|w| w.find_step(-1)));
        add(&["<Control>m"], Rc::new(|w| w.toggle_minimap()));
        add(&["<Control><Shift>f"], Rc::new(|w| w.toggle_toolbar()));
        add(&["<Control><Shift>e"], Rc::new(|w| w.toggle_source()));
        add(&["<Control>plus", "<Control>equal", "<Control>KP_Add"], Rc::new(|w| w.zoom(1.0)));
        add(&["<Control>minus", "<Control>KP_Subtract"], Rc::new(|w| w.zoom(-1.0)));
        add(&["<Control>0", "<Control>KP_0"], Rc::new(|w| w.zoom(0.0)));
        add(&["<Control>q"], Rc::new(|w| {
            w.flush();
            if let Some(app) = w.win.application() {
                app.quit();
            }
        }));
        add(&["<Control>b"], Rc::new(|w| w.inline("b")));
        add(&["<Control>i"], Rc::new(|w| w.inline("i")));
        add(&["<Control><Shift>x"], Rc::new(|w| w.inline("s")));
        add(&["<Control>e"], Rc::new(|w| w.inline("code")));
        add(&["<Control>k"], Rc::new(|w| w.link_popover()));
        add(&["<Control>backslash"], Rc::new(|w| w.clear()));
        add(&["<Control><Alt>0"], Rc::new(|w| w.block(BlockCmd::Para)));
        add(&["<Control><Alt>1"], Rc::new(|w| w.block(BlockCmd::Heading(1))));
        add(&["<Control><Alt>2"], Rc::new(|w| w.block(BlockCmd::Heading(2))));
        add(&["<Control><Alt>3"], Rc::new(|w| w.block(BlockCmd::Heading(3))));
        add(&["<Control><Alt>4"], Rc::new(|w| w.block(BlockCmd::Check)));
        add(&["<Control><Alt>5"], Rc::new(|w| w.block(BlockCmd::Bullet)));
        add(&["<Control><Alt>6"], Rc::new(|w| w.block(BlockCmd::Numbered)));
        add(&["<Control><Alt>8"], Rc::new(|w| w.block(BlockCmd::Code)));
        add(&["<Control><Alt>t"], Rc::new(|w| w.table()));
        add(&["<Control><Alt>q"], Rc::new(|w| w.block(BlockCmd::Quote)));
        add(&["<Control><Shift>v"], Rc::new(|w| {
            if let Some(t) = w.active_tab() {
                t.paste(true);
            }
        }));
        self.win.add_controller(ctrl);
    }
}
