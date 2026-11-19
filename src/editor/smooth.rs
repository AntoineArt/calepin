//! Scrolling. Over the text, GTK scrolls as in every other application: the
//! wheel and the touchpad behave as the system has them set up (speed,
//! inertia). Calepin only steps in where GTK cannot: the tables are drawn in
//! an overlay above the text, outside the scrolled window, so the scroll
//! events that reach them never get to it. Over a table the page is moved
//! here, with GTK's own steps: a wheel notch moves page_size^(2/3), the
//! touchpad moves 1:1, and a fling slows down as GTK's does.

use super::*;

/// Inertia as GtkScrolledWindow's: speed lost per second (e^-k·t), and the
/// speed it stops at (px/s).
const FRICTION: f64 = 4.0;
const MIN_SPEED: f64 = 12.0;
const MAX_SPEED: f64 = 12000.0;

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

        // Whether the touchpad gesture under way started over a table: only
        // then is its fling ours (over the text, GTK flings).
        let on_table = Rc::new(Cell::new(false));
        let flags = gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::KINETIC;
        let scroll = gtk::EventControllerScroll::new(flags);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        let (p, ot) = (pointer.clone(), on_table.clone());
        scroll.connect_scroll_begin(move |_| {
            if let Some(t) = weak.upgrade() {
                t.stop_motion();
                ot.set(p.get().is_some_and(|(x, y)| t.table_at(x, y).is_some()));
            }
        });
        let weak = Rc::downgrade(self);
        let p = pointer.clone();
        scroll.connect_scroll(move |c, dx, dy| match weak.upgrade() {
            Some(t) => t.on_scroll(c, dx, dy, p.get()),
            None => glib::Propagation::Proceed,
        });
        let weak = Rc::downgrade(self);
        let ot = on_table.clone();
        scroll.connect_decelerate(move |_, vx, vy| {
            if let Some(t) = weak.upgrade()
                && ot.replace(false)
                && vy.abs() > vx.abs()
            {
                t.fling(vy);
            }
        });
        self.overlay.add_controller(scroll);

        // A click or a key stops a fling where it is.
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

    /// The table under a point of the overlay, and its sideways adjustment
    /// when it is wider than its room.
    fn table_at(&self, x: f64, y: f64) -> Option<Option<gtk::Adjustment>> {
        self.tables.borrow().iter().find_map(|slot| {
            let b = slot.view.root.compute_bounds(&self.overlay)?;
            if !b.contains_point(&gtk::graphene::Point::new(x as f32, y as f32)) {
                return None;
            }
            let h = slot.view.scroller.hadjustment();
            Some((h.upper() - h.page_size() > 0.5).then_some(h))
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
        // Over the text: GTK's own scrolling, as everywhere else.
        let Some(hadj) = pointer.and_then(|(x, y)| self.table_at(x, y)) else {
            return glib::Propagation::Proceed;
        };
        self.stop_motion();
        // Shift+wheel, or a sideways swipe, scrolls a wide table sideways.
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let (sx, sy) = if shift { (dx + dy, 0.0) } else { (dx, dy) };
        let step = |adj: &gtk::Adjustment, d: f64| {
            let k = if wheel { adj.page_size().powf(2.0 / 3.0) } else { 1.0 };
            let max = (adj.upper() - adj.page_size()).max(adj.lower());
            adj.set_value((adj.value() + d * k).clamp(adj.lower(), max));
        };
        if sx != 0.0
            && let Some(h) = &hadj
        {
            step(h, sx);
        }
        if sy != 0.0 {
            step(&self.scrolled.vadjustment(), sy);
        }
        glib::Propagation::Stop
    }

    /// Fingers lifted while moving over a table: the page goes on and slows
    /// down.
    fn fling(&self, vy: f64) {
        if vy.abs() < MIN_SPEED * 4.0 {
            return;
        }
        self.scroll_vel.set(vy.clamp(-MAX_SPEED, MAX_SPEED));
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

    /// One frame of a fling; false once the page is at rest. Relative to
    /// where the view is: GTK may move it when it measures lines again.
    fn fling_frame(&self, now: i64) -> bool {
        let vel = self.scroll_vel.get();
        if vel == 0.0 {
            return false;
        }
        let last = self.scroll_clock.replace(now);
        let dt = if last == 0 { 1.0 / 60.0 } else { ((now - last) as f64 / 1e6).clamp(0.001, 0.1) };
        let adj = self.scrolled.vadjustment();
        let (lo, hi) = (adj.lower(), (adj.upper() - adj.page_size()).max(adj.lower()));
        let next = (adj.value() + vel * dt).clamp(lo, hi);
        adj.set_value(next);
        let vel = vel * (-FRICTION * dt).exp();
        self.scroll_vel.set(vel);
        vel.abs() >= MIN_SPEED && next > lo && next < hi
    }
}
