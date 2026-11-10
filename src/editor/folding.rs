//! Folding: a region's lines are hidden whole by the `fold` tag (invisible
//! text, still in the document and saved as is). Arrows in the left margin and a
//! "⋯" after a folded line are drawn in a layer above the text.

use super::*;
use crate::fold::{self, Region};

/// Where the arrows sit, left of a line's text.
const ARROW_GAP: f64 = 20.0;
const PILL_W: f64 = 26.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FoldMode {
    Rich,
    MarkdownSource,
    Json,
    Yaml,
}

impl Tab {
    pub(super) fn fold_mode(&self) -> Option<FoldMode> {
        if self.markdown.get() {
            return Some(FoldMode::Rich);
        }
        let path = self.path.borrow();
        let ext = path.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
        match ext.as_deref() {
            None if path.is_none() => Some(FoldMode::MarkdownSource),
            Some("md" | "markdown" | "mdown" | "mkd") => Some(FoldMode::MarkdownSource),
            Some("json" | "jsonc" | "json5" | "geojson" | "webmanifest") => Some(FoldMode::Json),
            Some("yml" | "yaml") => Some(FoldMode::Yaml),
            _ => None,
        }
    }

    pub(super) fn show_line_numbers(&self, show: bool) {
        self.line_numbers.set_visible(show);
        if show {
            self.size_line_numbers();
        }
    }

    /// Wide enough for the last line's number.
    fn size_line_numbers(&self) {
        let digits = self.buffer.line_count().max(1).to_string().len().max(2);
        let (w, _) = sourceview5::prelude::GutterRendererTextExt::measure(&self.line_numbers, &"9".repeat(digits));
        if self.line_numbers.width_request() != w + 16 {
            self.line_numbers.set_width_request(w + 16);
        }
    }

    pub(super) fn connect_folding(self: &Rc<Self>) {
        let numbers = &self.line_numbers;
        numbers.set_xalign(1.0);
        numbers.set_xpad(8);
        numbers.set_visible(false);
        numbers.add_css_class("line-numbers");
        sourceview5::prelude::ViewExt::gutter(&self.view, gtk::TextWindowType::Left).insert(numbers, 0);
        let me = Rc::downgrade(self);
        numbers.connect_query_data(move |r, lines, line| {
            let Some(t) = me.upgrade() else { return };
            let Some(lines) = lines.downcast_ref::<sourceview5::GutterLines>() else { return };
            let it = lines.iter_at_line(line);
            if it.has_tag(&t.fold_tag()) {
                r.set_text("");
            } else if lines.is_cursor(line) {
                r.set_markup(&format!("<b>{}</b>", line + 1));
            } else {
                r.set_text(&(line + 1).to_string());
            }
        });

        self.fold_area.set_can_target(false);
        self.fold_area.set_can_focus(false);
        self.overlay.add_overlay(&self.fold_area);
        let me = Rc::downgrade(self);
        self.fold_area.set_draw_func(move |_, cr, _, _| {
            if let Some(t) = me.upgrade() {
                t.draw_folds(cr);
            }
        });
        let area = self.fold_area.clone();
        self.scrolled.vadjustment().connect_value_changed(move |_| area.queue_draw());

        // Clicks on an arrow or a "⋯", before GtkTextView moves the caret.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.set_button(gdk::BUTTON_PRIMARY);
        let me = Rc::downgrade(self);
        click.connect_pressed(move |g, n, x, y| {
            let Some(t) = me.upgrade() else { return };
            if n != 1 {
                return;
            }
            if let Some(region) = t.fold_target_at(x, y) {
                g.set_state(gtk::EventSequenceState::Claimed);
                t.toggle_region(region);
            }
        });
        self.view.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        let me = Rc::downgrade(self);
        motion.connect_motion(move |_, x, y| {
            let Some(t) = me.upgrade() else { return };
            let line = t.line_at_widget_y(y);
            let margin = x < t.text_left(line.unwrap_or(0)) - 2.0;
            let hover = line.map(|l| (l, margin));
            if t.fold_hover.replace(hover) != hover {
                t.fold_area.queue_draw();
            }
            if t.fold_target_at(x, y).is_some() {
                t.view.set_cursor_from_name(Some("pointer"));
            }
        });
        let me = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            if let Some(t) = me.upgrade()
                && t.fold_hover.replace(None).is_some()
            {
                t.fold_area.queue_draw();
            }
        });
        self.view.add_controller(motion);
    }

    /// Regions are computed again shortly after edits.
    pub(super) fn schedule_fold_scan(&self) {
        if self.fold_scan_pending.replace(true) {
            return;
        }
        let me = self.me.borrow().clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            if let Some(t) = me.upgrade() {
                t.fold_scan_pending.set(false);
                t.fold_scan();
            }
        });
    }

    pub(super) fn fold_scan(&self) {
        let regions = match self.fold_mode() {
            None => Vec::new(),
            Some(FoldMode::Rich) => {
                let n = self.buffer.line_count();
                let mut levels = Vec::with_capacity(n as usize);
                let mut blank = Vec::with_capacity(n as usize);
                for l in 0..n {
                    let it = self.buffer.iter_at_line(l).unwrap_or(self.buffer.end_iter());
                    let level = it.tags().iter().find_map(|t| {
                        t.name().and_then(|n| n.strip_prefix('h').and_then(|d| d.parse::<u8>().ok()))
                    });
                    levels.push(level);
                    blank.push(it.ends_line() && self.line_anchor(l).is_none());
                }
                fold::sections(&levels, &blank)
            }
            Some(mode) => {
                let (s, e) = self.buffer.bounds();
                let text = self.buffer.text(&s, &e, true);
                match mode {
                    FoldMode::Json => fold::json(&text),
                    FoldMode::Yaml => fold::yaml(&text),
                    _ => fold::markdown(&text),
                }
            }
        };
        // Folds whose region went away (edited) or that no longer hide anything.
        let tag = self.fold_tag();
        for m in self.fold_marks.borrow().iter().filter(|m| !m.is_deleted()) {
            let line = self.buffer.iter_at_mark(m).line();
            let alive = regions.iter().any(|r| r.start == line)
                && self.buffer.iter_at_line(line + 1).is_some_and(|i| i.has_tag(&tag));
            if !alive {
                self.buffer.delete_mark(m);
            }
        }
        self.fold_marks.borrow_mut().retain(|m| !m.is_deleted());
        *self.fold_regions.borrow_mut() = regions;
        self.fold_area.queue_draw();
        if self.line_numbers.is_visible() {
            self.size_line_numbers();
        }
    }

    /// The hidden part of a region: its lines after the first, whole
    /// (a line left with only its newline visible would show empty).
    fn hidden_range(&self, r: Region) -> Option<(gtk::TextIter, gtk::TextIter)> {
        let n = self.buffer.line_count();
        if r.end >= n || r.end <= r.start {
            return None;
        }
        let s = self.buffer.iter_at_line(r.start + 1)?;
        let e = if r.end + 1 < n { self.buffer.iter_at_line(r.end + 1)? } else { self.buffer.end_iter() };
        (e.offset() > s.offset()).then_some((s, e))
    }

    fn fold_tag(&self) -> gtk::TextTag {
        self.tag("fold").expect("fold tag")
    }

    /// The mark heading a folded region on `line`, if any.
    fn fold_mark(&self, line: i32) -> Option<gtk::TextMark> {
        self.fold_marks
            .borrow()
            .iter()
            .find(|m| !m.is_deleted() && self.buffer.iter_at_mark(*m).line() == line)
            .cloned()
    }

    pub(super) fn is_folded(&self, r: Region) -> bool {
        self.fold_mark(r.start).is_some()
    }

    fn hide(&self, r: Region) {
        if let Some((s, e)) = self.hidden_range(r) {
            self.buffer.apply_tag(&self.fold_tag(), &s, &e);
        }
    }

    /// Marks `r` folded (its lines hidden) or not; unfolding keeps the
    /// regions folded inside it folded.
    fn mark_folded(&self, r: Region, folded: bool) {
        let Some((s, e)) = self.hidden_range(r) else { return };
        if folded {
            if self.fold_mark(r.start).is_none() {
                let m = self.buffer.create_mark(None, &self.buffer.iter_at_line(r.start).unwrap_or(s), true);
                self.fold_marks.borrow_mut().push(m);
            }
            self.buffer.apply_tag(&self.fold_tag(), &s, &e);
            return;
        }
        if let Some(m) = self.fold_mark(r.start) {
            self.buffer.delete_mark(&m);
        }
        self.fold_marks.borrow_mut().retain(|m| !m.is_deleted());
        self.buffer.remove_tag(&self.fold_tag(), &s, &e);
        let regions = self.fold_regions.borrow().clone();
        for inner in regions.iter().filter(|i| i.start > r.start && i.start <= r.end) {
            if self.is_folded(*inner) {
                self.hide(*inner);
            }
        }
    }

    fn set_folded(&self, r: Region, folded: bool) {
        if self.hidden_range(r).is_none() {
            return;
        }
        self.mark_folded(r, folded);
        if folded {
            // The caret does not stay hidden: to the end of the first line.
            if self.cursor().has_tag(&self.fold_tag()) {
                self.buffer.place_cursor(&self.line_bounds(r.start).1);
            }
        }
        self.after_fold_change();
    }

    fn after_fold_change(&self) {
        self.fold_area.queue_draw();
        self.overlay.queue_allocate();
        self.map.queue_draw();
        let cb = self.on_change.borrow().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    pub(super) fn toggle_region(&self, r: Region) {
        self.set_folded(r, !self.is_folded(r));
    }

    /// The region starting on `line`, or the innermost one containing it.
    fn region_for_line(&self, line: i32) -> Option<Region> {
        let regions = self.fold_regions.borrow();
        regions
            .iter()
            .find(|r| r.start == line)
            .or_else(|| regions.iter().filter(|r| r.start < line && line <= r.end).max_by_key(|r| r.start))
            .copied()
    }

    fn scan_if_pending(&self) {
        if self.fold_scan_pending.get() || self.fold_regions.borrow().is_empty() {
            self.fold_scan();
        }
    }

    pub fn fold_at_cursor(&self) {
        self.scan_if_pending();
        let line = self.cursor().line();
        // On a folded line, fold the enclosing region.
        let mut target = self.region_for_line(line);
        if let Some(r) = target
            && self.is_folded(r)
        {
            target = self.region_for_line(r.start - 1).filter(|o| o.start < r.start && o.end >= r.end);
        }
        if let Some(r) = target {
            self.set_folded(r, true);
        }
    }

    pub fn unfold_at_cursor(&self) {
        self.scan_if_pending();
        let line = self.cursor().line();
        let regions = self.fold_regions.borrow().clone();
        if let Some(r) = regions.iter().find(|r| r.start == line && self.is_folded(**r)) {
            self.set_folded(*r, false);
        }
    }

    pub fn fold_all(&self) {
        self.scan_if_pending();
        let regions = self.fold_regions.borrow().clone();
        for r in regions.iter() {
            self.mark_folded(*r, true);
        }
        if self.cursor().has_tag(&self.fold_tag()) {
            self.buffer.place_cursor(&self.buffer.start_iter());
        }
        self.after_fold_change();
    }

    pub fn unfold_all(&self) {
        for m in self.fold_marks.borrow_mut().drain(..) {
            if !m.is_deleted() {
                self.buffer.delete_mark(&m);
            }
        }
        let (s, e) = self.buffer.bounds();
        self.buffer.remove_tag(&self.fold_tag(), &s, &e);
        self.after_fold_change();
    }

    /// First lines of the folded regions (for the session and re-renders).
    pub fn folded_lines(&self) -> Vec<i32> {
        let mut lines: Vec<i32> = self
            .fold_marks
            .borrow()
            .iter()
            .filter(|m| !m.is_deleted())
            .map(|m| self.buffer.iter_at_mark(m).line())
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// Folds again the regions starting on `lines`, those that still exist.
    pub fn refold(&self, lines: &[i32]) {
        self.fold_marks.borrow_mut().retain(|m| !m.is_deleted());
        if lines.is_empty() {
            return;
        }
        self.fold_scan();
        let regions = self.fold_regions.borrow().clone();
        for r in regions.iter().filter(|r| lines.contains(&r.start)) {
            self.mark_folded(*r, true);
        }
        self.after_fold_change();
    }

    /// Unfolds what hides `it`: the region heading its hidden run, then any
    /// region folded inside that still hides it.
    pub(super) fn reveal(&self, it: &gtk::TextIter) {
        self.scan_if_pending();
        let tag = self.fold_tag();
        let off = it.offset();
        for _ in 0..64 {
            let it = self.buffer.iter_at_offset(off);
            if !it.has_tag(&tag) {
                break;
            }
            let mut start = it;
            if !start.starts_tag(Some(&tag)) {
                start.backward_to_tag_toggle(Some(&tag));
            }
            // The outermost folded region over `it` whose first line shows.
            let line = it.line();
            let regions = self.fold_regions.borrow().clone();
            let head = regions
                .iter()
                .filter(|r| r.start < line && line <= r.end && self.is_folded(**r))
                .filter(|r| self.buffer.iter_at_line(r.start).is_some_and(|h| !h.has_tag(&tag)))
                .min_by_key(|r| r.start)
                .copied();
            match head {
                Some(r) => self.mark_folded(r, false),
                None => {
                    // No region any more (edited away): drop the hidden run.
                    let mut end = start;
                    end.forward_to_tag_toggle(Some(&tag));
                    self.buffer.remove_tag(&tag, &start, &end);
                }
            }
        }
        self.after_fold_change();
    }

    /// The caret never rests in hidden text. At the start of a folded part
    /// it goes back to the end of the line above, or past the part when it
    /// came from there (stepping right); deeper inside, the part unfolds.
    pub(super) fn keep_caret_out_of_folds(&self) {
        if self.busy.get() > 0 || self.buffer.has_selection() {
            return;
        }
        let tag = self.fold_tag();
        let cur = self.cursor();
        if !cur.has_tag(&tag) {
            return;
        }
        let mut before = cur;
        let at_start = !before.backward_char() || !before.has_tag(&tag);
        if !at_start {
            self.reveal(&cur);
            return;
        }
        let mut start = cur;
        if !start.starts_tag(Some(&tag)) {
            start.backward_to_tag_toggle(Some(&tag));
        }
        let mut end = cur;
        end.forward_to_tag_toggle(Some(&tag));
        let target = if self.caret_offset.get() == before.offset() || start.offset() == 0 { end } else { before };
        self.caret_offset.set(target.offset());
        self.buffer.place_cursor(&target);
    }

    /// GTK's copy leaves hidden text out: with folds in the selection, the
    /// whole text is copied (and cut) here. False when GTK can do it.
    pub(super) fn copy_with_folds(&self, cut: bool) -> bool {
        let Some((mut s, mut e)) = self.buffer.selection_bounds() else { return false };
        let tag = self.fold_tag();
        let mut probe = s;
        let hidden = probe.has_tag(&tag) || (probe.forward_to_tag_toggle(Some(&tag)) && probe.offset() < e.offset());
        if !hidden {
            return false;
        }
        self.view.clipboard().set_text(&self.buffer.text(&s, &e, true));
        if cut && self.view.is_editable() {
            self.buffer.begin_user_action();
            self.buffer.delete_interactive(&mut s, &mut e, true);
            self.buffer.end_user_action();
        }
        true
    }

    /// Enter at the end of a folded line opens a new line after the folded
    /// part, which stays folded.
    pub(super) fn fold_enter(&self) -> bool {
        let cur = self.cursor();
        if self.buffer.has_selection() || !cur.ends_line() {
            return false;
        }
        self.scan_if_pending();
        let line = cur.line();
        let Some(r) = self.fold_regions.borrow().iter().find(|r| r.start == line).copied() else { return false };
        if !self.is_folded(r) {
            return false;
        }
        let n = self.buffer.line_count();
        self.buffer.begin_user_action();
        if r.end + 1 < n {
            let mut it = self.buffer.iter_at_line(r.end + 1).unwrap_or(self.buffer.end_iter());
            self.buffer.insert(&mut it, "\n");
            let at = self.buffer.iter_at_line(r.end + 1).unwrap_or(self.buffer.end_iter());
            self.buffer.place_cursor(&at);
        } else {
            let mut it = self.buffer.end_iter();
            self.buffer.insert(&mut it, "\n");
            self.buffer.place_cursor(&self.buffer.end_iter());
        }
        self.buffer.end_user_action();
        true
    }

    /// A deletion next to or inside hidden text unfolds it: what is edited
    /// stays in sight.
    pub(super) fn unfold_near(&self, offset: i32) {
        let tag = self.fold_tag();
        let it = self.buffer.iter_at_offset(offset);
        let mut prev = it;
        if it.has_tag(&tag) {
            self.reveal(&it);
        } else if prev.backward_char() && prev.has_tag(&tag) {
            self.reveal(&prev);
        }
    }

    // -----------------------------------------------------------------------
    // Geometry and drawing
    // -----------------------------------------------------------------------

    /// x of a line's text, in view widget coordinates.
    fn text_left(&self, line: i32) -> f64 {
        let it = self.buffer.iter_at_line(line).unwrap_or(self.buffer.start_iter());
        let rect = self.view.iter_location(&it);
        let (x, _) = self.view.buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y());
        x as f64
    }

    fn line_at_widget_y(&self, y: f64) -> Option<i32> {
        let (_, by) = self.view.window_to_buffer_coords(gtk::TextWindowType::Widget, 0, y as i32);
        let (it, _) = self.view.line_at_y(by);
        Some(it.line())
    }

    /// Top and height of a line's first display line, in widget coordinates.
    fn line_box(&self, line: i32) -> (f64, f64) {
        let it = self.buffer.iter_at_line(line).unwrap_or(self.buffer.start_iter());
        let rect = self.view.iter_location(&it);
        let (_, y) = self.view.buffer_to_window_coords(gtk::TextWindowType::Widget, 0, rect.y());
        (y as f64, rect.height().max(8) as f64)
    }

    /// Where a folded line's "⋯" goes: after its visible text.
    fn pill_x(&self, r: Region) -> Option<f64> {
        self.hidden_range(r)?;
        let (_, s) = self.line_bounds(r.start);
        let rect = self.view.iter_location(&s);
        let (x, _) = self.view.buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y());
        Some(x as f64 + 8.0)
    }

    /// The region whose arrow or "⋯" is under (x, y), view coordinates.
    fn fold_target_at(&self, x: f64, y: f64) -> Option<Region> {
        let line = self.line_at_widget_y(y)?;
        let r = self.fold_regions.borrow().iter().find(|r| r.start == line).copied()?;
        let left = self.text_left(line);
        if x >= left - ARROW_GAP - 6.0 && x < left - 2.0 {
            return Some(r);
        }
        if self.is_folded(r)
            && let Some(px) = self.pill_x(r)
            && x >= px
            && x < px + PILL_W
        {
            return Some(r);
        }
        None
    }

    fn draw_folds(&self, cr: &gtk::cairo::Context) {
        let regions = self.fold_regions.borrow();
        if regions.is_empty() {
            return;
        }
        // Widget coordinates of the view inside the drawing layer.
        let Some(origin) = self.view.compute_point(&self.fold_area, &gtk::graphene::Point::new(0.0, 0.0)) else { return };
        let (ox, oy) = (origin.x() as f64, origin.y() as f64);
        let (_, top) = self.view.window_to_buffer_coords(gtk::TextWindowType::Widget, 0, 0);
        let (_, bottom) = self.view.window_to_buffer_coords(gtk::TextWindowType::Widget, 0, self.view.height());
        let first = self.view.line_at_y(top).0.line();
        let last = self.view.line_at_y(bottom).0.line();
        let colors = self.map_colors.borrow();
        let (muted, accent, pill) = (colors[1], colors[2], colors[3]);
        let hover = self.fold_hover.get();
        let tag = self.fold_tag();
        let from = regions.partition_point(|r| r.start < first);
        for r in regions[from..].iter().take_while(|r| r.start <= last) {
            let head = self.buffer.iter_at_line(r.start).unwrap_or(self.buffer.start_iter());
            if head.has_tag(&tag) {
                continue; // Inside a folded region.
            }
            let folded = self.is_folded(*r);
            let shown = folded || hover.is_some_and(|(l, margin)| margin || l == r.start);
            if !shown {
                continue;
            }
            let (y, h) = self.line_box(r.start);
            let cx = ox + self.text_left(r.start) - ARROW_GAP / 2.0 - 4.0;
            let cy = oy + y + h / 2.0;
            let c = if folded { accent } else { muted };
            cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, if folded { 1.0 } else { 0.8 });
            let s = 4.0;
            if folded {
                // ▸
                cr.move_to(cx - s * 0.6, cy - s);
                cr.line_to(cx + s * 0.8, cy);
                cr.line_to(cx - s * 0.6, cy + s);
            } else {
                // ▾
                cr.move_to(cx - s, cy - s * 0.6);
                cr.line_to(cx + s, cy - s * 0.6);
                cr.line_to(cx, cy + s * 0.8);
            }
            cr.close_path();
            let _ = cr.fill();
            if folded && let Some(px) = self.pill_x(*r) {
                let (x, ph) = (ox + px, 14.0_f64.min(h));
                let py = cy - ph / 2.0;
                cr.set_source_rgba(pill.red() as f64, pill.green() as f64, pill.blue() as f64, 1.0);
                let rad = ph / 2.0;
                cr.new_sub_path();
                cr.arc(x + rad, py + rad, rad, std::f64::consts::FRAC_PI_2, 3.0 * std::f64::consts::FRAC_PI_2);
                cr.arc(x + PILL_W - rad, py + rad, rad, -std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2);
                cr.close_path();
                let _ = cr.fill();
                let fg = colors[0];
                cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, 0.7);
                for k in 0..3 {
                    cr.arc(x + PILL_W / 2.0 + (k as f64 - 1.0) * 5.0, cy, 1.4, 0.0, std::f64::consts::TAU);
                    let _ = cr.fill();
                }
            }
        }
    }
}
