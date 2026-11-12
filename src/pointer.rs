//! Pointer shapes. The names live here; each surface (the text, a table)
//! picks its shape in one place from its own state.

use gtk::prelude::*;

/// Over text that takes the caret.
pub const TEXT: &str = "text";
/// Over something that acts on a click.
pub const HAND: &str = "pointer";
/// While the middle click scrolls.
pub const SCROLL: &str = "all-scroll";
/// Over a column border that can be dragged.
pub const COL_RESIZE: &str = "col-resize";

/// The hand over a widget that acts on a click (buttons).
pub fn clickable<W: IsA<gtk::Widget>>(w: W) -> W {
    w.set_cursor_from_name(Some(HAND));
    w
}
