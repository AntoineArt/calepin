//! Scrolling, over the text and the tables alike. One controller on the
//! overlay takes every scroll event, so the page moves the same way wherever
//! the pointer is (the tables sit in an overlay outside the scrolled window:
//! GTK alone would not scroll over them).
//!
//! The wheel moves GTK's step, page_size^(2/3) per notch. The touchpad moves
//! the page by a set number of pixels per unit of finger travel, then flings
//! with inertia. GTK's own 2.5 px per unit and its short inertia (e^-4t) are
//! far from what browsers do with the same gesture: Chromium (and Electron)
//! move 12 px per unit and fling about 0.47 s × the release speed, up to
//! ~3000 px (measured, Chromium 155 on Wayland). People set their system's
//! touchpad speed against their browser, so GTK's numbers feel slow and
//! dead next to it: the default here is the browsers', and the right-click
//! menu offers slower ones, down to GTK's.
//!
//! Every step is relative to where the view is now: GtkTextView measures
//! its lines lazily and moves the view itself when a line turns out taller
//! or shorter than estimated, keeping the same text on screen.

use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

/// Touchpad speeds offered in the menu: page pixels per unit of finger
/// travel. The first is GTK's own, the last Chromium's.
pub const TOUCHPAD_SPEEDS: [(&str, f64); 4] =
    [("Slow (as GTK apps)", 2.5), ("Medium", 5.0), ("Fast", 8.0), ("Very fast (as Chromium)", 12.0)];
pub const DEFAULT_TOUCHPAD_SPEED: f64 = 12.0;

/// The speed in use, ×100.
static TOUCHPAD_SPEED: AtomicU32 = AtomicU32::new(1200);

pub fn set_touchpad_speed(px: f64) {
    TOUCHPAD_SPEED.store((px.clamp(0.5, 40.0) * 100.0).round() as u32, Ordering::Relaxed);
}

fn touchpad_speed() -> f64 {
    TOUCHPAD_SPEED.load(Ordering::Relaxed) as f64 / 100.0
}

/// Inertia: speed lost per second (e^-k·t, so a fling travels v/k), the
/// fastest single fling (it travels ~3000 px) and the speed it stops at.
const FRICTION: f64 = 2.1;
const MAX_FLING: f64 = 6400.0;
const STOP_SPEED: f64 = 4.0;
/// Consecutive flings add up, as in GTK: these are GtkScrolledWindow's.
const ACCUMULATION_FLOOR: f64 = 0.33;
const ACCUMULATION_CEIL: f64 = 1.0;
const ACCUMULATION_MAX: f64 = 6.0;

impl Tab {
    pub(super) fn connect_smooth_scroll(self: &Rc<Self>) {
        // The pointer over the overlay: which table, if any, is under it.
        let pointer = Rc::new(Cell::new(None::<(f64, f64)>));
        let motion = gtk::EventControllerMotion::new();
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        let p = pointer.clone();
        motion.connect_motion(move |_, x, y| p.set(Some((x, y))));
        let p = pointer.clone();
        motion.connect_enter(move |_, x, y| p.set(Some((x, y))));
        let p = pointer.clone();
        motion.connect_leave(move |_| p.set(None));
        self.overlay.add_controller(motion);

        // A fling caught by the fingers: its speed, added to the next fling.
        let carried = Rc::new(Cell::new(0.0f64));
        let flags = gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::KINETIC;
        let scroll = gtk::EventControllerScroll::new(flags);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        let c = carried.clone();
        scroll.connect_scroll_begin(move |_| {
            if let Some(t) = weak.upgrade() {
                c.set(t.scroll_vel.get());
                t.stop_motion();
            }
        });
        let weak = Rc::downgrade(self);
        let p = pointer.clone();
        scroll.connect_scroll(move |c, dx, dy| match weak.upgrade() {
            Some(t) => t.on_scroll(c, dx, dy, p.get()),
            None => glib::Propagation::Proceed,
        });
        let weak = Rc::downgrade(self);
        let c = carried.clone();
        scroll.connect_decelerate(move |sc, vx, vy| {
            let carried = c.replace(0.0);
            let Some(t) = weak.upgrade() else { return };
            if sc.current_event_state().intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK) || vy.abs() <= vx.abs() {
                return;
            }
            let k = if sc.unit() == gdk::ScrollUnit::Wheel { t.wheel_step() } else { touchpad_speed() };
            let v = (vy * k).clamp(-MAX_FLING, MAX_FLING);
            t.fling(accumulate(v, carried).clamp(-3.0 * MAX_FLING, 3.0 * MAX_FLING));
        });
        self.overlay.add_controller(scroll);

        // A click or a key stops a fling where it is.
        let stop = gtk::EventControllerLegacy::new();
        stop.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        let c = carried;
        stop.connect_event(move |_, ev| {
            if matches!(ev.event_type(), gdk::EventType::ButtonPress | gdk::EventType::KeyPress | gdk::EventType::TouchBegin)
                && let Some(t) = weak.upgrade()
            {
                c.set(0.0);
                t.stop_motion();
            }
            glib::Propagation::Proceed
        });
        self.overlay.add_controller(stop);
    }

    fn wheel_step(&self) -> f64 {
        self.scrolled.vadjustment().page_size().powf(2.0 / 3.0)
    }

    /// The sideways adjustment of the table under a point of the overlay,
    /// when that table is wider than its room.
    fn wide_table_at(&self, x: f64, y: f64) -> Option<gtk::Adjustment> {
        self.tables.borrow().iter().find_map(|slot| {
            let b = slot.view.root.compute_bounds(&self.overlay)?;
            if !b.contains_point(&gtk::graphene::Point::new(x as f32, y as f32)) {
                return None;
            }
            let h = slot.view.scroller.hadjustment();
            (h.upper() - h.page_size() > 0.5).then_some(h)
        })
    }

    fn on_scroll(&self, c: &gtk::EventControllerScroll, dx: f64, dy: f64, pointer: Option<(f64, f64)>) -> glib::Propagation {
        let wheel = c.unit() == gdk::ScrollUnit::Wheel;
        let state = c.current_event_state();
        if state.contains(gdk::ModifierType::CONTROL_MASK) {
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
        self.stop_motion();
        // Shift+wheel, or a sideways swipe, scrolls a wide table sideways.
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let (sx, sy) = if shift { (dy, dx) } else { (dx, dy) };
        let step = |adj: &gtk::Adjustment, d: f64| {
            let k = if wheel { adj.page_size().powf(2.0 / 3.0) } else { touchpad_speed() };
            let max = (adj.upper() - adj.page_size()).max(adj.lower());
            adj.set_value((adj.value() + d * k).clamp(adj.lower(), max));
        };
        if sx != 0.0
            && let Some(h) = pointer.and_then(|(x, y)| self.wide_table_at(x, y))
        {
            step(&h, sx);
        }
        if sy != 0.0 {
            step(&self.scrolled.vadjustment(), sy);
        }
        glib::Propagation::Stop
    }

    /// Fingers lifted while moving: the page goes on and slows down.
    fn fling(&self, vy: f64) {
        if vy.abs() < STOP_SPEED {
            return;
        }
        self.scroll_vel.set(vy);
        if self.scroll_ticking.replace(true) {
            return;
        }
        self.scroll_clock.set(0);
        let me = self.me.borrow().clone();
        self.view.add_tick_callback(move |_, clock| {
            let Some(t) = me.upgrade() else { return glib::ControlFlow::Break };
            if t.fling_frame(clock.frame_time()) {
                return glib::ControlFlow::Continue;
            }
            t.scroll_ticking.set(false);
            t.scroll_vel.set(0.0);
            glib::ControlFlow::Break
        });
    }

    /// Stops a fling under way.
    pub(super) fn stop_motion(&self) {
        self.scroll_vel.set(0.0);
    }

    /// One frame of a fling; false once the page is at rest. The distance
    /// covered (v/k·(1 − e^-k·dt)) is added to where the view is now.
    fn fling_frame(&self, now: i64) -> bool {
        let vel = self.scroll_vel.get();
        if vel == 0.0 {
            return false;
        }
        let last = self.scroll_clock.replace(now);
        let dt = if last == 0 { 1.0 / 60.0 } else { ((now - last) as f64 / 1e6).clamp(0.001, 0.1) };
        let decay = (-FRICTION * dt).exp();
        let adj = self.scrolled.vadjustment();
        let (lo, hi) = (adj.lower(), (adj.upper() - adj.page_size()).max(adj.lower()));
        let next = (adj.value() + vel / FRICTION * (1.0 - decay)).clamp(lo, hi);
        adj.set_value(next);
        let vel = vel * decay;
        self.scroll_vel.set(vel);
        vel.abs() >= STOP_SPEED && next > lo && next < hi
    }
}

/// GTK's fling accumulation: a fling in the same direction as the one the
/// fingers just caught, and about as fast, adds part of its speed.
fn accumulate(v: f64, last: f64) -> f64 {
    if last == 0.0 || (v >= 0.0) != (last >= 0.0) || v.abs() < last.abs() * ACCUMULATION_FLOOR {
        return v;
    }
    let min = last * ACCUMULATION_FLOOR;
    let max = last * ACCUMULATION_CEIL;
    v + last * ((v - min) / (max - min)).min(ACCUMULATION_MAX)
}

#[cfg(test)]
mod tests {
    use super::accumulate;

    #[test]
    fn flings_add_up_like_gtk() {
        // Opposite direction or much slower: no carry.
        assert_eq!(accumulate(-500.0, 1000.0), -500.0);
        assert_eq!(accumulate(100.0, 1000.0), 100.0);
        // As fast as the caught fling: its whole speed carries.
        assert!((accumulate(1000.0, 1000.0) - 2000.0).abs() < 1e-9);
        // Capped at 6× the caught speed.
        assert!((accumulate(10000.0, 1000.0) - 16000.0).abs() < 1e-9);
        // Nothing caught.
        assert_eq!(accumulate(700.0, 0.0), 700.0);
    }
}
