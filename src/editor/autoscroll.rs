//! Middle-click fast scrolling: a middle click sets an anchor and the text
//! scrolls, faster the further the pointer moves from it. A click (or
//! Escape) stops it; holding the button while moving stops on release.
//! Off, the middle click pastes the primary selection, as GTK does.

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

/// Middle click: true = fast scrolling, false = paste.
static MIDDLE_SCROLL: AtomicBool = AtomicBool::new(true);

pub fn middle_scroll() -> bool {
    MIDDLE_SCROLL.load(Ordering::Relaxed)
}

pub fn set_middle_scroll(on: bool) {
    MIDDLE_SCROLL.store(on, Ordering::Relaxed);
}

/// No scrolling this close to the anchor.
const DEAD_ZONE: f64 = 10.0;

#[derive(Clone, Copy)]
pub(super) struct AutoScroll {
    anchor: (f64, f64),
    pointer: (f64, f64),
    /// Moved away while the button was down: release stops.
    dragged: bool,
    held: bool,
    clock: i64,
}

impl Tab {
    pub(super) fn connect_autoscroll(self: &Rc<Self>) {
        // Middle button: start (or stop), before the text views paste.
        let middle = gtk::GestureClick::new();
        middle.set_button(gdk::BUTTON_MIDDLE);
        middle.set_propagation_phase(gtk::PropagationPhase::Capture);
        let me = Rc::downgrade(self);
        middle.connect_pressed(move |g, _, x, y| {
            let Some(t) = me.upgrade() else { return };
            if t.auto.get().is_some() {
                g.set_state(gtk::EventSequenceState::Claimed);
                t.stop_autoscroll();
            } else if middle_scroll() {
                g.set_state(gtk::EventSequenceState::Claimed);
                t.start_autoscroll(x, y);
            }
        });
        let me = Rc::downgrade(self);
        middle.connect_released(move |_, _, _, _| {
            let Some(t) = me.upgrade() else { return };
            if let Some(mut a) = t.auto.get() {
                if a.dragged {
                    t.stop_autoscroll();
                } else {
                    a.held = false;
                    t.auto.set(Some(a));
                }
            }
        });
        self.overlay.add_controller(middle);

        // Any other button stops it, without doing anything else.
        let other = gtk::GestureClick::new();
        other.set_button(0);
        other.set_propagation_phase(gtk::PropagationPhase::Capture);
        let me = Rc::downgrade(self);
        other.connect_pressed(move |g, _, _, _| {
            let Some(t) = me.upgrade() else { return };
            if g.current_button() != gdk::BUTTON_MIDDLE && t.auto.get().is_some() {
                g.set_state(gtk::EventSequenceState::Claimed);
                t.stop_autoscroll();
            }
        });
        self.overlay.add_controller(other);

        let motion = gtk::EventControllerMotion::new();
        let me = Rc::downgrade(self);
        motion.connect_motion(move |_, x, y| {
            let Some(t) = me.upgrade() else { return };
            if let Some(mut a) = t.auto.get() {
                a.pointer = (x, y);
                if a.held && ((x - a.anchor.0).abs() > DEAD_ZONE || (y - a.anchor.1).abs() > DEAD_ZONE) {
                    a.dragged = true;
                }
                t.auto.set(Some(a));
            }
        });
        self.overlay.add_controller(motion);

        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let me = Rc::downgrade(self);
        key.connect_key_pressed(move |_, k, _, _| {
            let Some(t) = me.upgrade() else { return glib::Propagation::Proceed };
            if t.auto.get().is_some() {
                t.stop_autoscroll();
                if k == gdk::Key::Escape {
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        self.overlay.add_controller(key);
    }

    fn start_autoscroll(&self, x: f64, y: f64) {
        self.scroll_target.set(None);
        let clock = self.overlay.frame_clock().map_or(0, |c| c.frame_time());
        self.auto.set(Some(AutoScroll { anchor: (x, y), pointer: (x, y), dragged: false, held: true, clock }));
        self.set_auto_cursor(Some("all-scroll"));
        self.fold_area.queue_draw();
        let me = self.me.borrow().clone();
        self.overlay.add_tick_callback(move |_, clock| {
            let Some(t) = me.upgrade() else { return glib::ControlFlow::Break };
            let Some(mut a) = t.auto.get() else { return glib::ControlFlow::Break };
            let now = clock.frame_time();
            let dt = ((now - a.clock) as f64 / 1_000_000.0).clamp(0.0, 0.1);
            a.clock = now;
            t.auto.set(Some(a));
            let d = a.pointer.1 - a.anchor.1;
            let over = (d.abs() - DEAD_ZONE).max(0.0);
            // Pixels per second: gentle near the anchor, fast far away.
            let speed = d.signum() * (over * 6.0 + over * over * 0.02);
            if speed != 0.0 {
                let adj = t.scrolled.vadjustment();
                let max = (adj.upper() - adj.page_size()).max(adj.lower());
                adj.set_value((adj.value() + speed * dt).clamp(adj.lower(), max));
            }
            glib::ControlFlow::Continue
        });
    }

    pub(super) fn stop_autoscroll(&self) {
        if self.auto.take().is_some() {
            self.set_auto_cursor(None);
            self.fold_area.queue_draw();
        }
    }

    /// The scroll cursor over the text and the tables, or back to normal.
    fn set_auto_cursor(&self, name: Option<&str>) {
        self.view.set_cursor_from_name(Some(name.unwrap_or("text")));
        for slot in self.tables.borrow().iter() {
            slot.view.set_cursor(name);
        }
    }

    /// The anchor: a disc with arrows up and down, drawn in the fold layer.
    pub(super) fn draw_autoscroll(&self, cr: &gtk::cairo::Context) {
        let Some(a) = self.auto.get() else { return };
        let colors = self.map_colors.borrow();
        let (fg, bg) = (colors[0], colors[4]);
        let (x, y) = a.anchor;
        cr.set_source_rgba(bg.red() as f64, bg.green() as f64, bg.blue() as f64, 0.85);
        cr.arc(x, y, 14.0, 0.0, std::f64::consts::TAU);
        let _ = cr.fill_preserve();
        cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, 0.6);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        cr.arc(x, y, 1.8, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
        for dir in [-1.0, 1.0] {
            cr.move_to(x - 4.5, y + dir * 4.0);
            cr.line_to(x, y + dir * 9.0);
            cr.line_to(x + 4.5, y + dir * 4.0);
            cr.close_path();
            let _ = cr.fill();
        }
    }
}
