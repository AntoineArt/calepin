//! Once per start: is there a newer release on GitHub? When Calepin runs from
//! a binary the user can replace (install.sh puts it in ~/.local/bin), the new
//! release is downloaded, checked against its SHA-256 and swapped in.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const API: &str = "https://api.github.com/repos/AntoineArt/calepin/releases/latest";
const PAGE: &str = "https://github.com/AntoineArt/calepin/releases/latest";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub page: String,
    /// Binary and checksum URLs for this machine, if published.
    asset: Option<(String, String)>,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    UpToDate,
    /// Newer, but this copy cannot replace itself (Flatpak, system package…).
    Available(Release),
    /// Newer and installed: a restart runs it.
    Installed(Release),
    Failed(Release, String),
}

pub fn disabled() -> bool {
    std::env::var_os("CALEPIN_NO_UPDATE").is_some()
}

/// Fetches a URL with curl (present on nearly every Linux system).
fn fetch(url: &str, out: Option<&Path>) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-fsSL", "--max-time", "60", "-H", "User-Agent: calepin-updater", "-H", "Accept: application/vnd.github+json"]);
    if let Some(p) = out {
        cmd.arg("-o").arg(p);
    }
    cmd.arg(url);
    let o = cmd.output().map_err(|e| format!("curl: {e}"))?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    Ok(o.stdout)
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().trim_start_matches('v').split('.').map(|p| p.split(|c: char| !c.is_ascii_digit()).next().unwrap_or("").parse::<u64>());
    Some((it.next()?.ok()?, it.next().unwrap_or(Ok(0)).ok()?, it.next().unwrap_or(Ok(0)).ok()?))
}

fn asset_name() -> Option<String> {
    (std::env::consts::OS == "linux").then(|| format!("calepin-{}-linux", std::env::consts::ARCH))
}

fn latest() -> Result<Release, String> {
    let api = std::env::var("CALEPIN_UPDATE_API").unwrap_or_else(|_| API.to_string());
    let body = fetch(&api, None)?;
    let json: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let version = json["tag_name"].as_str().ok_or("no tag_name")?.trim_start_matches('v').to_string();
    let page = json["html_url"].as_str().unwrap_or(PAGE).to_string();
    let url_of = |name: &str| {
        json["assets"].as_array()?.iter().find(|a| a["name"].as_str() == Some(name))?["browser_download_url"].as_str().map(str::to_string)
    };
    let asset = asset_name().and_then(|n| Some((url_of(&n)?, url_of(&format!("{n}.sha256"))?)));
    Ok(Release { version, page, asset })
}

/// The running binary, if the user may replace it.
fn replaceable_exe() -> Option<PathBuf> {
    if std::env::var_os("FLATPAK_ID").is_some() {
        return None;
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let dir = exe.parent()?;
    // A writable directory (the swap is a rename) and a binary owned by the
    // same user. (Opening a running binary for writing fails: ETXTBSY.)
    use std::os::unix::fs::MetadataExt;
    let probe = dir.join(format!(".calepin-update-probe-{}", std::process::id()));
    std::fs::File::create(&probe).ok()?;
    let me = std::fs::metadata(&probe).ok().map(|m| m.uid());
    let _ = std::fs::remove_file(&probe);
    (me == std::fs::metadata(&exe).ok().map(|m| m.uid())).then_some(exe)
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let o = Command::new("sha256sum").arg(path).output().map_err(|e| format!("sha256sum: {e}"))?;
    let s = String::from_utf8_lossy(&o.stdout);
    s.split_whitespace().next().map(str::to_string).ok_or_else(|| "sha256sum: no output".into())
}

fn install(binary: &str, checksum: &str, exe: &Path) -> Result<(), String> {
    let expected = String::from_utf8_lossy(&fetch(checksum, None)?).split_whitespace().next().unwrap_or("").to_lowercase();
    if expected.len() != 64 {
        return Err("checksum file unreadable".into());
    }
    let tmp = exe.with_extension(format!("update-{}", std::process::id()));
    let result = (|| {
        fetch(binary, Some(&tmp))?;
        let got = sha256_of(&tmp)?;
        if got != expected {
            return Err(format!("checksum mismatch ({got})"));
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        // Sanity: the new binary runs.
        let v = Command::new(&tmp).arg("--version").output().map_err(|e| e.to_string())?;
        if !v.status.success() {
            return Err("downloaded binary does not start".into());
        }
        // Atomic swap; the running process keeps its old inode.
        std::fs::rename(&tmp, exe).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Blocking: run off the main thread.
pub fn check_and_update() -> Outcome {
    let release = match latest() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("calepin: update check failed: {e}");
            return Outcome::UpToDate;
        }
    };
    let current = parse_version(env!("CARGO_PKG_VERSION"));
    if parse_version(&release.version) <= current {
        return Outcome::UpToDate;
    }
    let (Some(exe), Some((bin, sum))) = (replaceable_exe(), release.asset.clone()) else {
        return Outcome::Available(release);
    };
    match install(&bin, &sum, &exe) {
        Ok(()) => Outcome::Installed(release),
        Err(e) => Outcome::Failed(release, e),
    }
}

/// Replaces this process with the (new) binary, keeping its arguments out:
/// the window simply shows again.
pub fn restart() -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("calepin"));
    // current_exe() points at the replaced file name (" (deleted)" otherwise).
    let path = PathBuf::from(exe.to_string_lossy().trim_end_matches(" (deleted)"));
    let _ = std::io::stderr().flush();
    Command::new(path).exec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.10"), Some((1, 10, 0)));
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert_eq!(parse_version("x"), None);
    }
}
