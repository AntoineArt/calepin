//! Images in Markdown: `![alt](src)` shows the picture in the text, as one
//! character of the buffer (a paintable), scaled down to the text column.
//! Pictures are decoded off the main thread and kept in a small cache, so an
//! undo (which renders the document again) does not decode them again.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gtk::{gdk, gio, glib, graphene, gsk, prelude::*, subclass::prelude::*};

use crate::model::{Image, Width};

/// File extensions taken for images (drop, paste, the file chooser).
pub const EXTENSIONS: [&str; 11] = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif", "heic", "tif", "tiff"];

pub fn is_image_path(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ImagePaintable {
        pub texture: RefCell<Option<gdk::Texture>>,
        /// The picture's size, once known (before decoding for local files).
        pub natural: Cell<Option<(i32, i32)>>,
        pub max_width: Cell<i32>,
        /// The width asked for (`{width=…}`, or the drag under way).
        pub width: Cell<Option<Width>>,
        pub failed: Cell<bool>,
        pub selected: Cell<bool>,
        /// The pointer is on the resize handle (or dragging it).
        pub handle: Cell<bool>,
        /// Shown while loading or when the picture cannot be read.
        pub label: RefCell<String>,
        pub view: glib::WeakRef<gtk::Widget>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ImagePaintable {
        const NAME: &'static str = "CalepinImage";
        type Type = super::ImagePaintable;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for ImagePaintable {}

    impl PaintableImpl for ImagePaintable {
        fn intrinsic_width(&self) -> i32 {
            self.size().0
        }

        fn intrinsic_height(&self) -> i32 {
            self.size().1
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(snap) = snapshot.downcast_ref::<gtk::Snapshot>() else { return };
            let (w, h) = (width as f32, height as f32);
            let rect = graphene::Rect::new(0.0, 0.0, w, h);
            let view = self.view.upgrade();
            let fg = view.as_ref().map_or(gdk::RGBA::BLACK, |v| v.color());
            if let Some(tex) = self.texture.borrow().as_ref() {
                snap.append_scaled_texture(tex, gsk::ScalingFilter::Trilinear, &rect);
            } else {
                let rounded = gsk::RoundedRect::from_rect(rect, 6.0);
                let line = gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.35);
                snap.append_border(&rounded, &[1.0; 4], &[line; 4]);
                if let Some(v) = &view {
                    let layout = v.create_pango_layout(Some(&self.label.borrow()));
                    let (lw, lh) = layout.pixel_size();
                    snap.save();
                    snap.translate(&graphene::Point::new(((w - lw as f32) / 2.0).max(8.0), (h - lh as f32) / 2.0));
                    snap.append_layout(&layout, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.6));
                    snap.restore();
                }
            }
            if self.handle.get() && self.texture.borrow().is_some() {
                // The resize handle: a bar on the right edge.
                let bh = (h * 0.3).clamp(16.0, 48.0).min(h);
                let bar = graphene::Rect::new(w - 7.0, (h - bh) / 2.0, 4.0, bh);
                let rounded = gsk::RoundedRect::from_rect(bar, 2.0);
                snap.push_rounded_clip(&rounded);
                let c = view.as_ref().map_or(fg, accent);
                snap.append_color(&gdk::RGBA::new(c.red(), c.green(), c.blue(), 0.95), &bar);
                snap.pop();
                let outline = gsk::RoundedRect::from_rect(graphene::Rect::new(bar.x() - 1.0, bar.y() - 1.0, 6.0, bh + 2.0), 3.0);
                snap.append_border(&outline, &[1.0; 4], &[gdk::RGBA::new(0.0, 0.0, 0.0, 0.5); 4]);
            }
            if self.selected.get() {
                let sel = view.as_ref().map_or(fg, accent);
                snap.append_color(&gdk::RGBA::new(sel.red(), sel.green(), sel.blue(), 0.3), &rect);
            }
        }
    }

    impl ImagePaintable {
        /// The size drawn: the picture's, at most the column's width.
        pub fn size(&self) -> (i32, i32) {
            let max = self.max_width.get().max(48);
            match self.natural.get() {
                Some((w, h)) if !self.failed.get() && w > 0 && h > 0 => {
                    let want = match self.width.get() {
                        Some(Width::Px(px)) => px.round() as i32,
                        Some(Width::Percent(p)) => (max as f64 * p / 100.0).round() as i32,
                        None => w,
                    };
                    let shown = want.clamp(1, max);
                    if shown == w {
                        (w, h)
                    } else {
                        (shown, ((h as f64 * shown as f64 / w as f64).round() as i32).max(1))
                    }
                }
                _ => {
                    let (lw, lh) = self
                        .view
                        .upgrade()
                        .map(|v| v.create_pango_layout(Some(&self.label.borrow())).pixel_size())
                        .unwrap_or((120, 18));
                    ((lw + 28).min(max), lh + 20)
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct ImagePaintable(ObjectSubclass<imp::ImagePaintable>) @implements gdk::Paintable;
}

/// The selection colour: the view's accent where the theme gives one.
fn accent(v: &gtk::Widget) -> gdk::RGBA {
    #[allow(deprecated)]
    v.style_context().lookup_color("accent_bg_color").unwrap_or_else(|| v.color())
}

impl ImagePaintable {
    pub fn new(image: &Image, base: Option<&Path>, view: &impl IsA<gtk::Widget>, max_width: i32) -> ImagePaintable {
        let p: ImagePaintable = glib::Object::new();
        let imp = p.imp();
        imp.view.set(Some(view.upcast_ref()));
        imp.max_width.set(max_width);
        imp.width.set(image.width());
        let name = if image.alt.trim().is_empty() { image.location() } else { image.alt.clone() };
        *imp.label.borrow_mut() = format!("🖼  {name}");
        match resolve(&image.location(), base) {
            Some(source) => p.load(source),
            None => imp.failed.set(true),
        }
        p
    }

    pub fn set_max_width(&self, w: i32) {
        let imp = self.imp();
        if imp.max_width.replace(w) != w {
            self.invalidate_size();
        }
    }

    /// Shows the image `px` wide (a drag under way), or at the width it asks.
    pub fn set_width(&self, w: Option<Width>) {
        if self.imp().width.replace(w) != w {
            self.invalidate_size();
        }
    }

    /// The width drawn, and whether the picture is there to resize.
    pub fn shown_width(&self) -> Option<i32> {
        let imp = self.imp();
        (imp.natural.get().is_some() && !imp.failed.get()).then(|| imp.size().0)
    }

    /// The picture's own width, in pixels.
    pub fn natural_width(&self) -> Option<i32> {
        self.imp().natural.get().map(|(w, _)| w)
    }

    pub fn max_width(&self) -> i32 {
        self.imp().max_width.get().max(48)
    }

    pub fn set_handle(&self, on: bool) {
        if self.imp().handle.replace(on) != on {
            self.invalidate_contents();
        }
    }

    pub fn set_selected(&self, on: bool) {
        if self.imp().selected.replace(on) != on {
            self.invalidate_contents();
        }
    }

    fn set_texture(&self, tex: gdk::Texture) {
        let imp = self.imp();
        let size = (tex.width(), tex.height());
        *imp.texture.borrow_mut() = Some(tex);
        imp.failed.set(false);
        if imp.natural.replace(Some(size)) != Some(size) {
            self.invalidate_size();
        }
        self.invalidate_contents();
    }

    fn set_failed(&self) {
        let imp = self.imp();
        imp.failed.set(true);
        imp.natural.set(None);
        self.invalidate_size();
        self.invalidate_contents();
    }

    fn load(&self, source: Source) {
        let key = source.key();
        if let Some(tex) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
            self.set_texture(tex);
            return;
        }
        if let Source::Path(path) = &source
            && let Some((_, w, h)) = gtk::gdk_pixbuf::Pixbuf::file_info(path)
        {
            // Known before decoding: the text does not move once it shows.
            self.imp().natural.set(Some((w, h)));
        }
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let bytes = match source {
                Source::Path(path) => gio::spawn_blocking(move || std::fs::read(path).map(glib::Bytes::from_owned).ok()).await.ok().flatten(),
                Source::Uri(uri) => gio::File::for_uri(&uri).load_bytes_future().await.ok().map(|(b, _)| b),
            };
            let tex = match bytes {
                Some(b) => gio::spawn_blocking(move || gdk::Texture::from_bytes(&b).ok()).await.ok().flatten(),
                None => None,
            };
            let Some(p) = weak.upgrade() else { return };
            match tex {
                Some(tex) => {
                    CACHE.with(|c| {
                        let mut c = c.borrow_mut();
                        if c.len() >= 64 {
                            c.clear();
                        }
                        c.insert(key, tex.clone());
                    });
                    p.set_texture(tex);
                }
                None => p.set_failed(),
            }
        });
    }
}

thread_local! {
    static CACHE: RefCell<HashMap<String, gdk::Texture>> = RefCell::new(HashMap::new());
}

pub enum Source {
    Path(PathBuf),
    Uri(String),
}

impl Source {
    /// The cache key; a file changed on disk is read again.
    fn key(&self) -> String {
        match self {
            Source::Path(p) => {
                let mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
                format!("{}\u{1f}{mtime:?}", p.display())
            }
            Source::Uri(u) => u.clone(),
        }
    }

    /// Where the image opens on Ctrl+click.
    pub fn uri(&self) -> String {
        match self {
            Source::Path(p) => gio::File::for_path(p).uri().to_string(),
            Source::Uri(u) => u.clone(),
        }
    }
}

/// Where `loc` points: relative to the note's folder `base`, or absolute,
/// `~/…`, `file://…`, `http(s)://…`.
pub fn resolve(loc: &str, base: Option<&Path>) -> Option<Source> {
    let lower = loc.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(Source::Uri(loc.to_string()));
    }
    if lower.starts_with("file://") {
        return gio::File::for_uri(loc).path().map(Source::Path);
    }
    if loc.contains("://") || lower.starts_with("data:") {
        return None;
    }
    let candidates = std::iter::once(loc.to_string()).chain(glib::Uri::unescape_string(loc, None::<&str>).map(|s| s.to_string()).filter(|s| s != loc));
    let mut first = None;
    for c in candidates {
        let p = if let Some(rest) = c.strip_prefix("~/") {
            glib::home_dir().join(rest)
        } else if Path::new(&c).is_absolute() {
            PathBuf::from(&c)
        } else {
            base?.join(&c)
        };
        if p.exists() {
            return Some(Source::Path(p));
        }
        first.get_or_insert(p);
    }
    first.map(Source::Path)
}

/// How a note in `base` refers to the file `path`: relative when inside the
/// note's folder, absolute otherwise; in angle brackets when it has spaces.
pub fn reference(path: &Path, base: Option<&Path>) -> String {
    let rel = base.and_then(|b| path.strip_prefix(b).ok()).filter(|r| !r.as_os_str().is_empty());
    let s = rel.unwrap_or(path).to_string_lossy().to_string();
    if s.contains([' ', '(', ')', '<', '>']) { format!("<{s}>") } else { s }
}

/// The image Markdown for a file.
pub fn image_for(path: &Path, base: Option<&Path>) -> Image {
    let alt = path.file_stem().map(|s| s.to_string_lossy().replace(['[', ']'], "")).unwrap_or_default();
    Image { alt, src: reference(path, base), attrs: None }
}

/// Saves a pasted picture as PNG: in `images/` next to the note, or in
/// Calepin's data folder for a note never saved to a file (or kept in a
/// folder that cannot be written).
pub fn save_pasted(tex: &gdk::Texture, base: Option<&Path>) -> std::io::Result<PathBuf> {
    let bytes = tex.save_to_png_bytes();
    let fallback = glib::user_data_dir().join("calepin").join("images");
    match base {
        Some(b) => save_png_in(&b.join("images"), &bytes).or_else(|_| save_png_in(&fallback, &bytes)),
        None => save_png_in(&fallback, &bytes),
    }
}

fn save_png_in(dir: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = glib::DateTime::now_local().ok().and_then(|d| d.format("%Y%m%d-%H%M%S").ok()).map(|s| s.to_string()).unwrap_or_else(|| "image".into());
    let mut path = dir.join(format!("image-{stamp}.png"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("image-{stamp}-{n}.png"));
        n += 1;
    }
    std::fs::write(&path, bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references() {
        let base = Path::new("/notes");
        assert_eq!(reference(Path::new("/notes/images/a.png"), Some(base)), "images/a.png");
        assert_eq!(reference(Path::new("/pics/a b.png"), Some(base)), "</pics/a b.png>");
        assert_eq!(reference(Path::new("/pics/a.png"), None), "/pics/a.png");
        let img = image_for(Path::new("/notes/my [shot].png"), Some(base));
        assert_eq!(img.markdown(), "![my shot](<my [shot].png>)");
        assert_eq!(crate::model::parse(&img.markdown())[0].spans[0].style.image.as_ref(), Some(&img));
    }

    #[test]
    fn resolves() {
        let base = Path::new("/notes");
        assert!(matches!(resolve("a.png", Some(base)), Some(Source::Path(p)) if p == Path::new("/notes/a.png")));
        assert!(matches!(resolve("/x/a.png", None), Some(Source::Path(p)) if p == Path::new("/x/a.png")));
        assert!(resolve("a.png", None).is_none());
        assert!(matches!(resolve("https://x.org/a.png", None), Some(Source::Uri(_))));
        assert!(matches!(resolve("file:///x/a%20b.png", None), Some(Source::Path(p)) if p == Path::new("/x/a b.png")));
        assert!(matches!(resolve("my%20pic.png", Some(base)), Some(Source::Path(p)) if p == Path::new("/notes/my%20pic.png")));
    }
}
