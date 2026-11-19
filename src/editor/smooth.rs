//! Scrolling with the wheel and the touchpad, over the text and the tables
//! alike. One controller on the overlay takes every scroll event, so the page
//! moves the same way wherever the pointer is: a short eased glide per wheel
//! notch, the fingers' movement 1:1 on a touchpad, then inertia after a fling.
//!
//! Every step is relative to where the view is now. GtkTextView measures its
//! lines lazily and moves the adjustment itself when a line turns out taller
//! or shorter than estimated (keeping the same text on screen); GTK's own
//! kinetic scrolling works on absolute positions and fought those
//! corrections, so the page sped up, slowed down or stopped dead.

use super::*;

/// Inertia: speed lost per second (e^-k·t), and the speed it stops at (px/s).
const FRICTION: f64 = 3.2;
const MIN_SPEED: f64 = 12.0;
const MAX_SPEED: f64 = 9000.0;

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

        let flags = gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::KINETIC;
        let scroll = gtk::EventControllerScroll::new(flags);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        scroll.connect_scroll_begin(move |_| {
            // Fingers back on the touchpad: the fling stops.
            if let Some(t) = weak.upgrade() {
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
        scroll.connect_decelerate(move |_, vx, vy| {
            if let Some(t) = weak.upgrade()
                && vy.abs() > vx.abs()
            {
                t.fling(vy);
            }
        });
        self.overlay.add_controller(scroll);

        // A click or a key stops the page where it is.
        let stop = gtk::EventControllerLegacy::new();
        stop.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        stop.connect_event(move |_, ev| {
            if matches!(ev.event_type(), gdk::EventType::ButtonPress | gdk::EventType::KeyPress | gdk::EventType::TouchBegin)
                && let Some(t) = weak.upgrade()
            {
                t.stop_motion();
            }
            glib::Propagation::Proceed
        });
        self.overlay.add_controller(stop);
    }

    /// The table under a point of the overlay, if it scrolls sideways.
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
        // Shift+wheel, or a sideways swipe, scrolls a wide table sideways.
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let (sx, sy) = if shift { (dx + dy, 0.0) } else { (dx, dy) };
        if sx != 0.0
            && let Some((x, y)) = pointer
            && let Some(h) = self.wide_table_at(x, y)
        {
            let step = if wheel { h.page_size().powf(2.0 / 3.0) } else { 1.0 };
            let max = (h.upper() - h.page_size()).max(h.lower());
            h.set_value((h.value() + sx * step).clamp(h.lower(), max));
        }
        if sy != 0.0 {
            if wheel {
                self.wheel(sy);
            } else {
                // Touchpad: the page follows the fingers.
                self.stop_motion();
                let adj = self.scrolled.vadjustment();
                let max = (adj.upper() - adj.page_size()).max(adj.lower());
                adj.set_value((adj.value() + sy).clamp(adj.lower(), max));
            }
        }
        glib::Propagation::Stop
    }

    /// One wheel notch (or part of one, on a high-resolution wheel).
    fn wheel(&self, dy: f64) {
        let adj = self.scrolled.vadjustment();
        let step = adj.page_size().powf(2.0 / 3.0);
        self.scroll_vel.set(0.0);
        let base = self.scroll_target.get().unwrap_or(adj.value());
        let max = (adj.upper() - adj.page_size()).max(adj.lower());
        self.scroll_target.set(Some((base + dy * step).clamp(adj.lower(), max)));
        self.run_motion();
    }

    /// Fingers lifted while moving: the page goes on and slows down.
    fn fling(&self, vy: f64) {
        if vy.abs() < MIN_SPEED * 4.0 {
            return;
        }
        self.scroll_target.set(None);
        self.scroll_vel.set(vy.clamp(-MAX_SPEED, MAX_SPEED));
        self.run_motion();
    }

    /// Stops a wheel glide or a fling under way.
    pub(super) fn stop_motion(&self) {
        self.scroll_target.set(None);
        self.scroll_vel.set(0.0);
    }

    fn run_motion(&self) {
        if self.scroll_ticking.replace(true) {
            return;
        }
        self.scroll_clock.set(0);
        self.scroll_set.set(self.scrolled.vadjustment().value());
        let me = self.me.borrow().clone();
        self.view.add_tick_callback(move |_, clock| {
            let Some(t) = me.upgrade() else { return glib::ControlFlow::Break };
            let go = t.motion_frame(clock.frame_time());
            if !go {
                t.scroll_ticking.set(false);
                t.stop_motion();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    /// One frame of motion; false once the page is at rest.
    fn motion_frame(&self, now: i64) -> bool {
        let last = self.scroll_clock.replace(now);
        let dt = if last == 0 { 1.0 / 60.0 } else { ((now - last) as f64 / 1e6).clamp(0.001, 0.1) };
        let adj = self.scrolled.vadjustment();
        let v = adj.value();
        let (lo, hi) = (adj.lower(), (adj.upper() - adj.page_size()).max(adj.lower()));
        if let Some(target) = self.scroll_target.get() {
            // GTK moved the view itself (lines measured again, the same text
            // kept on screen): the target moves with it.
            let target = (target + v - self.scroll_set.get()).clamp(lo, hi);
            // Ease out: cover ~90 % of the distance in 120 ms.
            let next = v + (target - v) * (1.0 - (-dt / 0.052).exp());
            if (target - next).abs() < 0.5 {
                adj.set_value(target);
                return false;
            }
            self.scroll_target.set(Some(target));
            adj.set_value(next);
            self.scroll_set.set(adj.value());
            return true;
        }
        let vel = self.scroll_vel.get();
        if vel == 0.0 {
            return false;
        }
        let next = (v + vel * dt).clamp(lo, hi);
        adj.set_value(next);
        self.scroll_set.set(adj.value());
        let vel = vel * (-FRICTION * dt).exp();
        self.scroll_vel.set(vel);
        vel.abs() >= MIN_SPEED && next > lo && next < hi
    }
}
