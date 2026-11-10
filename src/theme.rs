//! Omarchy theme integration: colours from the active theme's `colors.toml`,
//! the system monospace font from fontconfig, and live reload on change.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::{gdk, gio, glib, prelude::*};

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    colors: HashMap<String, String>,
    pub font: String,
}

fn home() -> PathBuf {
    glib::home_dir()
}

pub fn theme_state_dir() -> PathBuf {
    home().join(".local/state/omarchy/current")
}

fn fontconfig_dir() -> PathBuf {
    // Inside Flatpak the config dir is the sandbox's: use the host's.
    std::env::var_os("HOST_XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| if std::env::var_os("FLATPAK_ID").is_some() { home().join(".config") } else { glib::user_config_dir() })
        .join("fontconfig")
}

// ---------------------------------------------------------------------------
// Without Omarchy: the desktop's light/dark preference and accent colour,
// from the XDG settings portal.
// ---------------------------------------------------------------------------

fn portal_read(conn: &gio::DBusConnection, key: &str) -> Option<glib::Variant> {
    let call = |method: &str| {
        conn.call_sync(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            method,
            Some(&("org.freedesktop.appearance", key).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            300,
            gio::Cancellable::NONE,
        )
        .ok()
    };
    // ReadOne returns (v); the older Read returns (v) holding another v.
    let reply = call("ReadOne").or_else(|| call("Read"))?;
    let mut v = reply.child_value(0);
    while v.type_() == glib::VariantTy::VARIANT {
        v = v.as_variant()?;
    }
    Some(v)
}

/// (dark, accent) as the desktop prefers them.
fn desktop_preferences() -> (bool, Option<String>) {
    let env_dark = std::env::var("GTK_THEME").map(|t| t.to_lowercase().contains("dark")).unwrap_or(false);
    let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return (env_dark, None) };
    let dark = portal_read(&conn, "color-scheme").and_then(|v| v.get::<u32>()).map(|n| n == 1).unwrap_or(env_dark);
    let accent = portal_read(&conn, "accent-color").and_then(|v| v.get::<(f64, f64, f64)>()).and_then(|(r, g, b)| {
        let ok = |x: f64| (0.0..=1.0).contains(&x);
        (ok(r) && ok(g) && ok(b)).then(|| format!("#{:02x}{:02x}{:02x}", (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8))
    });
    (dark, accent)
}

/// A neutral palette (close to GNOME's) for desktops without Omarchy.
fn fallback_colors(dark: bool, accent: Option<String>) -> HashMap<String, String> {
    let pairs: &[(&str, &str)] = if dark {
        &[
            ("background", "#1e1e1e"),
            ("dark_background", "#262626"),
            ("darker_background", "#303030"),
            ("lighter_background", "#2a2a2a"),
            ("foreground", "#e6e6e6"),
            ("bright_foreground", "#ffffff"),
            ("dark_foreground", "#b0b0b0"),
            ("muted", "#8a8a8a"),
            ("selection", "#2f4d73"),
            ("accent", "#78aeed"),
            ("red", "#ff7b63"),
            ("green", "#8ff0a4"),
            ("yellow", "#f8e45c"),
            ("orange", "#ffa348"),
            ("blue", "#78aeed"),
            ("magenta", "#dc8add"),
            ("cyan", "#93ddc2"),
        ]
    } else {
        &[
            ("background", "#fafafa"),
            ("dark_background", "#f0f0f0"),
            ("darker_background", "#e3e3e3"),
            ("lighter_background", "#ffffff"),
            ("foreground", "#1f1f1f"),
            ("bright_foreground", "#000000"),
            ("dark_foreground", "#4a4a4a"),
            ("muted", "#787878"),
            ("selection", "#c4dbf7"),
            ("accent", "#1c71d8"),
            ("red", "#c01c28"),
            ("green", "#26a269"),
            ("yellow", "#9c6e03"),
            ("orange", "#c64600"),
            ("blue", "#1c71d8"),
            ("magenta", "#9141ac"),
            ("cyan", "#0e7a7a"),
        ]
    };
    let mut map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    if let Some(a) = accent {
        map.insert("accent".into(), a);
    }
    map.insert("mode".into(), if dark { "dark" } else { "light" }.into());
    map
}

fn parse_colors(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        let v = v.split(" #").next().unwrap_or(v).trim().trim_matches('"');
        map.insert(k.trim().to_string(), v.to_string());
    }
    map
}

/// First family that the user's fontconfig prepends to `monospace`.
fn system_monospace() -> String {
    let path = fontconfig_dir().join("fonts.conf");
    if let Ok(text) = std::fs::read_to_string(path)
        && let Some(i) = text.find("<edit name=\"family\"")
        && let Some(s) = text[i..].find("<string>")
    {
        let rest = &text[i + s + 8..];
        if let Some(e) = rest.find("</string>") {
            let fam = rest[..e].trim();
            if !fam.is_empty() {
                return fam.to_string();
            }
        }
    }
    "monospace".to_string()
}

fn valid_hex(v: &str) -> bool {
    let h = v.trim_start_matches('#');
    v.starts_with('#') && (h.len() == 6 || h.len() == 8) && h.chars().all(|c| c.is_ascii_hexdigit())
}

fn rgb(hex: &str) -> [f64; 3] {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).unwrap_or(0) as f64 / 255.0;
    [p(0), p(2), p(4)]
}

fn luminance(c: [f64; 3]) -> f64 {
    let l = |x: f64| if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
    0.2126 * l(c[0]) + 0.7152 * l(c[1]) + 0.0722 * l(c[2])
}

/// WCAG contrast ratio between two colours.
fn contrast(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `color` moved toward `toward` until it reads at `ratio` on `bg`.
fn legible_over(color: &str, toward: &str, bg: &str, ratio: f64) -> String {
    let (c, t, b) = (rgb(color), rgb(toward), rgb(bg));
    for step in 0..=20 {
        let k = step as f64 / 20.0;
        let m = [0, 1, 2].map(|i| c[i] + (t[i] - c[i]) * k);
        if contrast(m, b) >= ratio || step == 20 {
            return format!("#{:02x}{:02x}{:02x}", (m[0] * 255.0).round() as u8, (m[1] * 255.0).round() as u8, (m[2] * 255.0).round() as u8);
        }
    }
    unreachable!()
}

impl Theme {
    pub fn load() -> Theme {
        let text = std::fs::read_to_string(theme_state_dir().join("theme/colors.toml")).unwrap_or_default();
        let mut colors = parse_colors(&text);
        if !colors.contains_key("background") {
            let (dark, accent) = desktop_preferences();
            colors = fallback_colors(dark, accent);
        }
        let dark = colors.get("mode").map(|m| m != "light").unwrap_or(true);
        Theme { dark, colors, font: system_monospace() }
    }

    /// A colour by Omarchy key, falling back through sensible alternatives.
    pub fn c(&self, key: &str) -> String {
        let chain: &[&str] = match key {
            "background" => &["background", "color0"],
            "foreground" => &["foreground", "color7"],
            "accent" => &["accent", "blue", "color4"],
            "selection" => &["selection", "selection_background", "lighter_background"],
            "muted" => &["muted", "dark_foreground", "color8"],
            "dark_background" => &["dark_background", "lighter_background", "background"],
            "darker_background" => &["darker_background", "dark_background", "background"],
            "lighter_background" => &["lighter_background", "dark_background", "background"],
            "dark_foreground" => &["dark_foreground", "muted", "foreground"],
            "bright_foreground" => &["bright_foreground", "foreground"],
            "red" => &["red", "color1"],
            "green" => &["green", "color2"],
            "yellow" => &["yellow", "color3"],
            "blue" => &["blue", "color4"],
            "magenta" => &["magenta", "color5"],
            "cyan" => &["cyan", "color6"],
            "orange" => &["orange", "bright_yellow", "yellow"],
            _ => &[],
        };
        for k in chain.iter().copied().chain(std::iter::once(key)) {
            if let Some(v) = self.colors.get(k)
                && valid_hex(v)
            {
                return v[..7].to_string();
            }
        }
        let dark = self.dark;
        match key {
            "background" | "dark_background" | "darker_background" | "lighter_background" => {
                if dark { "#1a1b26" } else { "#f5f5f5" }
            }
            "foreground" | "bright_foreground" => {
                if dark { "#c0caf5" } else { "#1f2328" }
            }
            "muted" | "dark_foreground" => "#808080",
            "selection" => {
                if dark { "#33467c" } else { "#c7dbff" }
            }
            _ => "#7aa2f7",
        }
        .to_string()
    }

    /// A secondary colour used for text, brought toward the foreground when
    /// the theme's own is too faint to read on the background (Omarchy's
    /// `muted` is often meant for decoration).
    pub fn legible(&self, key: &str, ratio: f64) -> String {
        legible_over(&self.c(key), &self.c("foreground"), &self.c("background"), ratio)
    }

    pub fn legible_rgba(&self, key: &str, ratio: f64) -> gdk::RGBA {
        gdk::RGBA::parse(self.legible(key, ratio)).unwrap_or(gdk::RGBA::BLACK)
    }

    pub fn rgba(&self, key: &str) -> gdk::RGBA {
        gdk::RGBA::parse(self.c(key)).unwrap_or(gdk::RGBA::BLACK)
    }

    /// `key` blended over the background, as an opaque colour.
    pub fn mix(&self, key: &str, amount: f32) -> gdk::RGBA {
        let a = self.rgba("background");
        let b = self.rgba(key);
        gdk::RGBA::new(
            a.red() + (b.red() - a.red()) * amount,
            a.green() + (b.green() - a.green()) * amount,
            a.blue() + (b.blue() - a.blue()) * amount,
            1.0,
        )
    }

    pub fn css(&self, font_size: f64) -> String {
        let bg = self.c("background");
        let fg = self.c("foreground");
        let accent = self.c("accent");
        // Tab names, counts, table controls: secondary, but text.
        let muted = self.legible("muted", 3.0);
        let dbg = self.c("dark_background");
        let ddbg = self.c("darker_background");
        let sel = self.c("selection");
        let font = &self.font;
        format!(
            r#"
window.calepin, window.calepin .mw-root {{ background: {bg}; color: {fg}; }}
window.calepin textview, window.calepin textview text {{
  background: {bg}; color: {fg};
  font-family: "{font}", monospace; font-size: {font_size}pt;
}}
window.calepin textview text selection {{ background: {sel}; color: {fg}; }}
window.calepin .tabbar {{ background: {dbg}; padding: 4px 6px 0 6px; }}
window.calepin .tab {{
  padding: 3px 4px 3px 12px; margin-right: 2px; border-radius: 7px 7px 0 0;
  color: {muted}; background: transparent;
}}
window.calepin .tab:hover {{ background: alpha({bg}, 0.5); color: {fg}; }}
window.calepin .tab.active {{ background: {bg}; color: {fg}; }}
window.calepin .tab label {{ font-family: "{font}", monospace; font-size: 10pt; }}
window.calepin .tab .close {{
  min-width: 18px; min-height: 18px; padding: 0; margin-left: 6px;
  border-radius: 5px; background: transparent; box-shadow: none; border: none;
  color: {muted}; opacity: 0;
}}
window.calepin .tab:hover .close, window.calepin .tab.active .close {{ opacity: 1; }}
window.calepin .tab .close:hover {{ background: {ddbg}; color: {fg}; }}
window.calepin .tab .dirty {{ color: {accent}; margin-left: 4px; font-size: 9pt; }}
window.calepin .tab .file {{ color: {muted}; margin-right: 6px; -gtk-icon-size: 12px; }}
window.calepin .tab .file.modified {{ color: {accent}; }}
window.calepin .newtab {{
  min-width: 26px; min-height: 22px; padding: 0; margin: 0 0 4px 2px;
  background: transparent; box-shadow: none; border: none; color: {muted}; border-radius: 6px;
}}
window.calepin .newtab:hover {{ background: {bg}; color: {fg}; }}
window.calepin .toolbar {{
  background: {bg}; padding: 3px 10px; border-bottom: 1px solid {dbg};
}}
window.calepin .toolbar button {{
  min-height: 24px; min-width: 28px; padding: 0 7px; margin: 0 1px;
  border-radius: 5px; background: transparent; box-shadow: none; border: none;
  color: {fg}; font-family: "{font}", monospace; font-size: 10pt;
}}
window.calepin .toolbar button:hover {{ background: {dbg}; }}
window.calepin .toolbar button:active {{ background: {ddbg}; }}
window.calepin .toolbar button:disabled {{ color: {muted}; opacity: 0.5; }}
window.calepin .toolbar .sep {{ background: {dbg}; min-width: 1px; margin: 4px 6px; }}
window.calepin .toolbar .count {{ color: {muted}; font-size: 9pt; margin-left: 8px; }}
window.calepin label.version {{ color: {muted}; opacity: 0.55; font-size: 8pt; margin: 2px 8px 4px 8px; }}
window.calepin .updatebar {{ background: alpha({accent}, 0.14); padding: 6px 12px; border-bottom: 1px solid {dbg}; }}
window.calepin .updatebar button {{ background: transparent; box-shadow: none; border: 1px solid alpha({accent}, 0.5); color: {fg}; border-radius: 6px; padding: 2px 10px; }}
window.calepin .findbar {{ background: {dbg}; padding: 4px 10px; border-top: 1px solid {ddbg}; }}
window.calepin .findbar entry {{ background: {bg}; color: {fg}; min-height: 26px; }}
window.calepin .findbar button {{ background: transparent; box-shadow: none; border: none; color: {fg}; }}
window.calepin .findbar label {{ color: {muted}; font-size: 9pt; }}
window.calepin popover contents {{ background: {dbg}; color: {fg}; }}
window.calepin popover entry {{ background: {bg}; color: {fg}; }}
window.calepin scrollbar slider {{ background: alpha({muted}, 0.6); }}
window.calepin scrollbar {{ background: transparent; }}
window.calepin .mw-table {{ margin: 4px 0 6px 0; }}
window.calepin .mw-grid {{ border: 1px solid {ddbg}; border-radius: 6px; }}
window.calepin textview.mw-cell, window.calepin textview.mw-cell text {{
  background: {bg}; color: {fg};
  font-family: "{font}", monospace; font-size: {font_size}pt;
}}
window.calepin textview.mw-cell {{ border-right: 1px solid {ddbg}; border-bottom: 1px solid {ddbg}; }}
window.calepin textview.mw-cell.last-col {{ border-right: none; }}
window.calepin textview.mw-cell.last-row {{ border-bottom: none; }}
window.calepin textview.mw-cell.header, window.calepin textview.mw-cell.header text {{ background: {dbg}; font-weight: bold; }}
window.calepin textview.mw-cell:focus-within, window.calepin textview.mw-cell:focus-within text {{ background: alpha({accent}, 0.10); }}
window.calepin textview.mw-cell text selection {{ background: alpha({accent}, 0.40); color: {fg}; }}
window.calepin .mw-table.selected .mw-grid {{ border-color: {sel}; }}
window.calepin .mw-table.selected textview.mw-cell, window.calepin .mw-table.selected textview.mw-cell text {{ background: {sel}; }}
window.calepin .mw-table-controls button {{
  min-height: 20px; padding: 0 6px; background: transparent; box-shadow: none; border: none;
  color: {muted}; font-size: 9pt; border-radius: 4px;
}}
window.calepin .mw-table-controls button:hover {{ background: {dbg}; color: {fg}; }}
window.calepin textview.minimap, window.calepin textview.minimap text {{
  background: {bg}; font-size: 1.6pt; font-family: "{font}", monospace;
}}
window.calepin .minimap {{ border-left: 1px solid {dbg}; }}
window.calepin .minimap slider {{ background: alpha({accent}, 0.14); }}
"#
        )
    }

    /// A GtkSourceView style scheme for this palette (selection, cursor,
    /// search, syntax highlighting for non-Markdown files).
    pub fn scheme_xml(&self, id: &str) -> String {
        let k = |k: &str| self.c(k);
        let current_line = {
            let c = self.mix("foreground", 0.05);
            format!("#{:02x}{:02x}{:02x}", (c.red() * 255.0) as u8, (c.green() * 255.0) as u8, (c.blue() * 255.0) as u8)
        };
        format!(
            r##"<?xml version="1.0" encoding="UTF-8"?>
<style-scheme id="{id}" name="Calepin Omarchy" version="1.0">
  <style name="text" foreground="{fg}" background="{bg}"/>
  <style name="selection" background="{sel}"/>
  <style name="selection-unfocused" background="{sel}"/>
  <style name="cursor" foreground="{accent}"/>
  <style name="current-line" background="{current_line}"/>
  <style name="line-numbers" foreground="{muted}" background="{bg}"/>
  <style name="current-line-number" foreground="{fg}" background="{current_line}" bold="true"/>
  <style name="bracket-match" background="{sel}" bold="true"/>
  <style name="bracket-mismatch" foreground="{red}" underline="single"/>
  <style name="search-match" foreground="{bg}" background="{accent}"/>
  <style name="map-overlay" background="{accent}"/>
  <style name="def:comment" foreground="{muted}" italic="true"/>
  <style name="def:shebang" foreground="{muted}" bold="true"/>
  <style name="def:doc-comment-element" foreground="{muted}" bold="true"/>
  <style name="def:keyword" foreground="{magenta}"/>
  <style name="def:builtin" foreground="{magenta}"/>
  <style name="def:statement" foreground="{magenta}"/>
  <style name="def:type" foreground="{cyan}"/>
  <style name="def:function" foreground="{blue}"/>
  <style name="def:identifier" foreground="{fg}"/>
  <style name="def:string" foreground="{green}"/>
  <style name="def:special-char" foreground="{cyan}"/>
  <style name="def:character" foreground="{green}"/>
  <style name="def:constant" foreground="{orange}"/>
  <style name="def:number" foreground="{orange}"/>
  <style name="def:boolean" foreground="{orange}"/>
  <style name="def:preprocessor" foreground="{red}"/>
  <style name="def:operator" foreground="{cyan}"/>
  <style name="def:error" foreground="{red}" underline="error"/>
  <style name="def:warning" foreground="{yellow}"/>
  <style name="def:note" foreground="{accent}" bold="true"/>
  <style name="def:net-address" foreground="{accent}" underline="single"/>
  <style name="def:link-destination" foreground="{accent}" underline="single"/>
  <style name="def:heading" foreground="{accent}" bold="true"/>
  <style name="def:emphasis" italic="true"/>
  <style name="def:strong-emphasis" bold="true"/>
  <style name="def:inline-code" foreground="{magenta}"/>
  <style name="def:preformatted-section" foreground="{magenta}"/>
  <style name="def:list-marker" foreground="{accent}" bold="true"/>
  <style name="def:insertion" foreground="{green}"/>
  <style name="def:deletion" foreground="{red}"/>
</style-scheme>
"##,
            fg = k("foreground"),
            bg = k("background"),
            sel = k("selection"),
            accent = k("accent"),
            muted = self.legible("muted", 3.0),
            red = k("red"),
            green = k("green"),
            yellow = k("yellow"),
            blue = k("blue"),
            magenta = k("magenta"),
            cyan = k("cyan"),
            orange = k("orange"),
        )
    }
}

/// Writes the scheme for `theme` and returns it, freshly loaded.
pub fn install_scheme(theme: &Theme, generation: u32) -> Option<sourceview5::StyleScheme> {
    let dir = glib::user_cache_dir().join("calepin/styles");
    std::fs::create_dir_all(&dir).ok()?;
    // Remove earlier generations so the manager never serves a stale scheme.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let id = format!("calepin-omarchy-{generation}");
    std::fs::write(dir.join(format!("{id}.xml")), theme.scheme_xml(&id)).ok()?;
    let manager = sourceview5::StyleSchemeManager::default();
    let dir_str = dir.to_string_lossy().to_string();
    if !manager.search_path().iter().any(|p| p.as_str() == dir_str) {
        manager.append_search_path(&dir_str);
    }
    manager.force_rescan();
    manager.scheme(&id)
}

/// Watches the Omarchy theme state and fontconfig; calls `on_change` with the
/// new theme after things settle. Returned monitors must be kept alive.
pub fn watch(on_change: impl Fn(Theme) + 'static) -> Vec<gio::FileMonitor> {
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let on_change = Rc::new(on_change);
    let mut monitors = Vec::new();
    for dir in [theme_state_dir(), fontconfig_dir()] {
        let file = gio::File::for_path(&dir);
        let Ok(monitor) = file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) else {
            continue;
        };
        let pending = pending.clone();
        let on_change = on_change.clone();
        monitor.connect_changed(move |_, _, _, _| {
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
            let pending2 = pending.clone();
            let on_change = on_change.clone();
            let id = glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                pending2.borrow_mut().take();
                on_change(Theme::load());
            });
            *pending.borrow_mut() = Some(id);
        });
        monitors.push(monitor);
    }
    // Desktop light/dark or accent changes (when not themed by Omarchy).
    if let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        let on_change = on_change.clone();
        #[allow(deprecated)]
        let id = conn.signal_subscribe(
            Some("org.freedesktop.portal.Desktop"),
            Some("org.freedesktop.portal.Settings"),
            Some("SettingChanged"),
            Some("/org/freedesktop/portal/desktop"),
            Some("org.freedesktop.appearance"),
            gio::DBusSignalFlags::NONE,
            move |_, _, _, _, _, _| on_change(Theme::load()),
        );
        // The subscription lasts as long as the connection (the process).
        let _ = id;
    }
    monitors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faint_colours_become_legible() {
        // Catppuccin Latte's muted on its background: about 1.9:1.
        let c = legible_over("#acb0be", "#4c4f69", "#eff1f5", 3.0);
        assert!(contrast(rgb(&c), rgb("#eff1f5")) >= 3.0);
        // Already legible: unchanged.
        assert_eq!(legible_over("#808080", "#ffffff", "#000000", 3.0), "#808080");
    }
}
