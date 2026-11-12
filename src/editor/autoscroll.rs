//! Middle-click fast scrolling: a middle click sets an anchor and the text
//! scrolls, faster the further the pointer moves from it; over a table
//! wider than its room, moving sideways scrolls the table sideways. A click (or
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
    /// Over a table that scrolls sideways.
    sideways: bool,
}

/// Pixels per second for a distance from the anchor: gentle near it, fast
/// far away.
fn speed(d: f64) -> f64 {
    let over = (d.abs() - DEAD_ZONE).max(0.0);
    d.signum() * (over * 6.0 + over * over * 0.02)
}

impl Tab {
    pub(super) fn connect_autoscroll(self: &Rc<Self>) {
        // Middle button: start (or stop), before the text views paste. A
        // drag gesture, not a click one: while the button is held the window
        // keeps the pointer, motion controllers see nothing, and only the
        // gesture holding the press follows the pointer.
        let middle = gtk::GestureDrag::new();
        middle.set_button(gdk::BUTTON_MIDDLE);
        middle.set_propagation_phase(gtk::PropagationPhase::Capture);
        let me = Rc::downgrade(self);
        middle.connect_drag_begin(move |g, x, y| {
            let Some(t) = me.upgrade() else { return };
            if t.auto.get().is_some() {
                // A second middle click ends sticky scrolling.
                g.set_state(gtk::EventSequenceState::Claimed);
                t.stop_autoscroll();
                t.auto_stopping.set(true);
            } else if middle_scroll() {
                g.set_state(gtk::EventSequenceState::Claimed);
                t.start_autoscroll(x, y);
            }
        });
        let me = Rc::downgrade(self);
        middle.connect_drag_update(move |_, dx, dy| {
            let Some(t) = me.upgrade() else { return };
            if let Some(mut a) = t.auto.get()
                && a.held
            {
                a.pointer = (a.anchor.0 + dx, a.anchor.1 + dy);
                if dx.abs() > DEAD_ZONE || dy.abs() > DEAD_ZONE {
                    a.dragged = true;
                }
                t.auto.set(Some(a));
            }
        });
        let me = Rc::downgrade(self);
        middle.connect_drag_end(move |_, _, _| {
            let Some(t) = me.upgrade() else { return };
            if t.auto_stopping.replace(false) {
                return;
            }
            if let Some(mut a) = t.auto.get() {
                if a.dragged {
                    // Held and moved: releasing stops.
                    t.stop_autoscroll();
                } else {
                    // A click: scrolling goes on, following the pointer.
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
            if let Some(mut a) = t.auto.get()
                && !a.held
            {
                a.pointer = (x, y);
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

        // Leaving the tab (another tab shown) stops it.
        let me = Rc::downgrade(self);
        self.overlay.connect_unmap(move |_| {
            if let Some(t) = me.upgrade() {
                t.stop_autoscroll();
            }
        });
    }

    fn start_autoscroll(&self, x: f64, y: f64) {
        self.scroll_target.set(None);
        let clock = self.overlay.frame_clock().map_or(0, |c| c.frame_time());
        // The table under the anchor, if it is wider than its room.
        let table = self.tables.borrow().iter().find_map(|slot| {
            let b = slot.view.root.compute_bounds(&self.overlay)?;
            let inside = b.contains_point(&gtk::graphene::Point::new(x as f32, y as f32));
            let h = slot.view.scroller.hadjustment();
            (inside && h.upper() > h.page_size() + 1.0).then(|| slot.view.clone())
        });
        let sideways = table.is_some();
        *self.auto_table.borrow_mut() = table;
        // A press anywhere in the window (tabs, toolbar) stops it, and so
        // does the window losing the focus: it never runs on unseen.
        if let Some(win) = self.view.root().and_downcast::<gtk::Window>() {
            let press = gtk::GestureClick::new();
            press.set_button(0);
            press.set_propagation_phase(gtk::PropagationPhase::Capture);
            let me = self.me.borrow().clone();
            press.connect_pressed(move |g, _, _, _| {
                if let Some(t) = me.upgrade()
                    && t.auto.get().is_some()
                    && g.current_button() != gdk::BUTTON_MIDDLE
                {
                    t.stop_autoscroll();
                }
            });
            win.add_controller(press.clone());
            let me = self.me.borrow().clone();
            let active = win.connect_is_active_notify(move |w| {
                if !w.is_active()
                    && let Some(t) = me.upgrade()
                {
                    t.stop_autoscroll();
                }
            });
            *self.auto_window.borrow_mut() = Some((win, press.upcast(), active));
        }
        self.auto.set(Some(AutoScroll { anchor: (x, y), pointer: (x, y), dragged: false, held: true, clock, sideways }));
        self.set_scroll_pointer(true);
        self.fold_area.queue_draw();
        let me = self.me.borrow().clone();
        self.overlay.add_tick_callback(move |_, clock| {
            let Some(t) = me.upgrade() else { return glib::ControlFlow::Break };
            let Some(mut a) = t.auto.get() else { return glib::ControlFlow::Break };
            let now = clock.frame_time();
            let dt = ((now - a.clock) as f64 / 1_000_000.0).clamp(0.0, 0.1);
            a.clock = now;
            t.auto.set(Some(a));
            let scroll = |adj: &gtk::Adjustment, v: f64| {
                if v != 0.0 {
                    let max = (adj.upper() - adj.page_size()).max(adj.lower());
                    adj.set_value((adj.value() + v * dt).clamp(adj.lower(), max));
                }
            };
            scroll(&t.scrolled.vadjustment(), speed(a.pointer.1 - a.anchor.1));
            if let Some(table) = t.auto_table.borrow().as_ref() {
                scroll(&table.scroller.hadjustment(), speed(a.pointer.0 - a.anchor.0));
            }
            glib::ControlFlow::Continue
        });
    }

    pub(super) fn stop_autoscroll(&self) {
        self.auto_table.borrow_mut().take();
        if let Some((win, press, active)) = self.auto_window.borrow_mut().take() {
            win.disconnect(active);
            // Not while the press that stops it is being handled: removing
            // the controller then would cancel that press for its target.
            glib::idle_add_local_once(move || win.remove_controller(&press));
        }
        if self.auto.take().is_some() {
            self.set_scroll_pointer(false);
            self.fold_area.queue_draw();
        }
    }

    /// The scroll shape over the text and the tables, or back to normal.
    fn set_scroll_pointer(&self, on: bool) {
        self.update_pointer();
        for slot in self.tables.borrow().iter() {
            slot.view.set_scrolling(on);
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
            if a.sideways {
                cr.move_to(x + dir * 4.0, y - 4.5);
                cr.line_to(x + dir * 9.0, y);
                cr.line_to(x + dir * 4.0, y + 4.5);
                cr.close_path();
                let _ = cr.fill();
            }
        }
    }
}
