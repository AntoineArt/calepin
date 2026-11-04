mod editor;
mod find;
mod model;
mod session;
mod table;
mod theme;
mod window;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::{gio, glib, prelude::*};

thread_local! {
    static WINDOW: RefCell<Option<Rc<window::Window>>> = const { RefCell::new(None) };
    /// The process stays resident: closing the window only hides it, so the
    /// next SUPER+N shows it at once.
    static HOLD: RefCell<Option<gio::ApplicationHoldGuard>> = const { RefCell::new(None) };
}

fn window(app: &gtk::Application) -> Rc<window::Window> {
    WINDOW.with(|w| {
        if let Some(win) = w.borrow().as_ref() {
            return win.clone();
        }
        let win = window::Window::new(app);
        let weak = Rc::downgrade(&win);
        win.win.connect_destroy(move |_| {
            if weak.upgrade().is_some() {
                WINDOW.with(|w| w.borrow_mut().take());
            }
        });
        *w.borrow_mut() = Some(win.clone());
        win
    })
}

static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// `CALEPIN_TIMING=1` prints startup milestones.
pub fn mark(what: &str) {
    if std::env::var_os("CALEPIN_TIMING").is_some() {
        let t = START.get_or_init(std::time::Instant::now).elapsed();
        eprintln!("calepin: {:>6.1} ms  {what}", t.as_secs_f64() * 1000.0);
    }
}

fn main() -> glib::ExitCode {
    START.get_or_init(std::time::Instant::now);
    let app = gtk::Application::builder()
        .application_id(window::APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_startup(|_| {
        mark("startup");
        sourceview5::init();
        gtk::Window::set_default_icon_name(window::APP_ID);
    });

    app.connect_command_line(|app, cmd| {
        mark("command line");
        HOLD.with(|h| {
            if h.borrow().is_none() {
                *h.borrow_mut() = Some(app.hold());
            }
        });
        let win = window(app);
        mark("window built");
        win.win.add_tick_callback(|_, _| {
            mark("first frame");
            glib::ControlFlow::Break
        });
        let mut background = false;
        // Where the result shows: the notes window, or a detached window
        // that already has the file.
        let mut target = win.clone();
        for arg in cmd.arguments().iter().skip(1) {
            let s = arg.to_string_lossy();
            match s.as_ref() {
                "-n" | "--new" => win.new_tab(),
                // Session autostart: load everything, show nothing.
                "--background" => background = true,
                "-h" | "--help" => {
                    println!("Usage: calepin [--new] [--background] [FICHIER…]");
                }
                _ => {
                    if let Some(p) = cmd.create_file_for_arg(arg).path() {
                        target = win.open_path(&p);
                    }
                }
            }
        }
        if background {
            return glib::ExitCode::SUCCESS;
        }
        target.win.present();
        // A hidden window gives the focus to its first focusable widget when
        // it maps (a table cell): put it back in the text.
        let w = target.clone();
        glib::idle_add_local_once(move || w.focus_text());
        if cmd.is_remote() && !target.detached {
            reveal_on_hyprland();
        }
        glib::ExitCode::SUCCESS
    });

    // Flush everything on termination signals (logout, kill, Ctrl+C).
    for sig in [libc_sig::SIGTERM, libc_sig::SIGINT, libc_sig::SIGHUP] {
        let app = app.clone();
        glib_unix::unix_signal_add_local(sig, move || {
            WINDOW.with(|w| {
                if let Some(win) = w.borrow().as_ref() {
                    win.flush();
                }
            });
            app.quit();
            glib::ControlFlow::Break
        });
    }

    app.run()
}

/// The window may sit on a hidden special workspace (the SUPER+N notes
/// scratchpad): ask Hyprland to focus it, which shows that workspace.
fn reveal_on_hyprland() {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    // The notes window is the one whose title starts with "Calepin".
    let expr = "hl.dsp.focus({ window = \"title:^Calepin.*$\" })".to_string();
    // In the background: the window shows meanwhile.
    std::thread::spawn(move || {
        let _ = window::host_command(&["hyprctl", "dispatch", &expr]).output();
    });
}

mod libc_sig {
    pub const SIGHUP: i32 = 1;
    pub const SIGINT: i32 = 2;
    pub const SIGTERM: i32 = 15;
}
