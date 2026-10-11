//! "Out": getting canvas assets into other apps (docs/ASSETS-V1.md §4, §7 desktop commands).
//!
//! - [`materialize`]: a human-named copy of a content-addressed asset in `<project>/.out/`,
//!   made with APFS `clonefile` (copy-on-write, instant, no extra space) and falling back to
//!   a plain copy. **Never a hardlink**: the receiving app may edit the file in place, which
//!   would corrupt the store (`<project>/assets/<sha256>.<ext>`).
//! - [`copy_files`] / [`copy_image`]: file URLs / PNG on the general pasteboard (objc2
//!   `NSPasteboard writeObjects`), so ⌘V works in Finder, Slack, Premiere, Mail.
//! - [`open_with_list`] / [`open_with`]: apps that open a file (Launch Services) plus pinned
//!   creative apps; [`drag_icon`]: the preview image for drag-out.
//!
//! Everything here is plain functions (unit-testable without a window); `main.rs` wraps
//! them as Tauri commands.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value as Json;

/// xattr on materialized copies: `<sha256 hex>:<size>`, so a later call reuses the copy.
const XATTR: &str = "ai.fal.os.sha256";

/// `~/.fal-os` (`$FALOS_HOME` wins), like the host's `home_dir`.
pub fn falos_home() -> Option<PathBuf> {
    std::env::var_os("FALOS_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".fal-os")))
}

/// The host discovery file (`host.json`), if a host runs.
pub fn host_file() -> Option<Json> {
    serde_json::from_slice(&std::fs::read(falos_home()?.join("host.json")).ok()?).ok()
}

/// `sha256:<hex>` or `<hex>` → lowercase hex.
pub fn parse_sha(id: &str) -> Option<String> {
    let hex = id.strip_prefix("sha256:").unwrap_or(id).to_ascii_lowercase();
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

fn valid_project_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && !id.starts_with('-')
}

/// Project folders to look in, the preferred one first: `<workspace>/<project>` when a
/// project id is given, the host's default project, then every project of the workspace.
pub fn project_dirs(host: &Json, project: Option<&str>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let ws = host.get("workspace").and_then(Json::as_str).map(PathBuf::from);
    if let (Some(ws), Some(p)) = (&ws, project.filter(|p| valid_project_id(p))) {
        out.push(ws.join(p));
    }
    if let Some(p) = host.get("project").and_then(Json::as_str) {
        out.push(PathBuf::from(p));
    }
    if let Some(ws) = &ws
        && let Ok(rd) = std::fs::read_dir(ws)
    {
        for e in rd.flatten() {
            let p = e.path();
            if p.join("project.json").is_file() {
                out.push(p);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// The stored file for a digest in `<dir>/assets/` (`<hex>.<ext>`).
pub fn find_asset(project_dir: &Path, hex: &str) -> Option<PathBuf> {
    let prefix = format!("{hex}.");
    std::fs::read_dir(project_dir.join("assets"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| (n == hex || n.starts_with(&prefix)) && !n.contains(".part"))
        })
}

/// A file name stem safe on APFS and in other apps: no separators or control characters,
/// no leading dot, at most 120 chars.
pub fn safe_stem(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c == '/' || c == ':' || c == '\\' || c.is_control() { '-' } else { c })
        .collect();
    let s = s.trim().trim_start_matches('.').trim();
    let s: String = s.chars().take(120).collect();
    s.trim().to_string()
}

#[cfg(target_os = "macos")]
fn clone_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let s = CString::new(src.as_os_str().as_bytes())?;
    let d = CString::new(dst.as_os_str().as_bytes())?;
    // SAFETY: valid NUL-terminated paths; clonefile does not retain them.
    let r = unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 0) };
    if r == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

#[cfg(not(target_os = "macos"))]
fn clone_file(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

#[cfg(target_os = "macos")]
fn get_tag(p: &Path) -> Option<String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(p.as_os_str().as_bytes()).ok()?;
    let name = CString::new(XATTR).ok()?;
    let mut buf = [0u8; 128];
    // SAFETY: buffer and length match; XATTR_NOFOLLOW keeps symlinks out.
    let n = unsafe {
        libc::getxattr(path.as_ptr(), name.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, libc::XATTR_NOFOLLOW)
    };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(target_os = "macos")]
fn set_tag(p: &Path, v: &str) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let (Ok(path), Ok(name)) = (CString::new(p.as_os_str().as_bytes()), CString::new(XATTR)) else { return };
    // SAFETY: valid pointers and length.
    unsafe {
        libc::setxattr(path.as_ptr(), name.as_ptr(), v.as_ptr().cast(), v.len(), 0, libc::XATTR_NOFOLLOW);
    }
}

#[cfg(not(target_os = "macos"))]
fn get_tag(_p: &Path) -> Option<String> {
    None
}
#[cfg(not(target_os = "macos"))]
fn set_tag(_p: &Path, _v: &str) {}

/// Clone (or copy) `src` to a fresh `dst` without ever hardlinking. The copy is written to a
/// temporary name first so a half-written file never appears under the human name.
fn clone_or_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    let tmp = dst.with_file_name(format!(
        ".{}.tmp{}",
        dst.file_name().and_then(|n| n.to_str()).unwrap_or("out"),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    if clone_file(src, &tmp).is_err() {
        // Other volume / filesystem without clones: a real copy (std uses fcopyfile here,
        // never a link).
        std::fs::copy(src, &tmp)?;
    }
    // The store's files may be read-only; the copy belongs to the user.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644));
    }
    // `rename` never replaces a different file here: callers pass a free name.
    std::fs::rename(&tmp, dst)
}

/// Materializes asset `src` (digest `hex`) as `<out_dir>/<name>.<ext>`. Reuses an earlier copy
/// of the same bytes; another file with that name gets a Downloads-style ` (1)` suffix.
pub fn materialize_file(src: &Path, hex: &str, out_dir: &Path, name: Option<&str>) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut stem = safe_stem(name.unwrap_or(""));
    if !ext.is_empty()
        && let Some(s) = stem.strip_suffix(&format!(".{ext}"))
    {
        stem = s.to_string();
    }
    if stem.is_empty() {
        stem = hex[..12].to_string();
    }
    let size = std::fs::metadata(src)?.len();
    let tag = format!("{hex}:{size}");
    for i in 0..1000 {
        let file = match (i, ext.is_empty()) {
            (0, true) => stem.clone(),
            (0, false) => format!("{stem}.{ext}"),
            (_, true) => format!("{stem} ({i})"),
            (_, false) => format!("{stem} ({i}).{ext}"),
        };
        let dst = out_dir.join(file);
        match std::fs::symlink_metadata(&dst) {
            Ok(m) => {
                // Our earlier copy of the same bytes, untouched: reuse it.
                if m.is_file() && m.len() == size && get_tag(&dst).as_deref() == Some(tag.as_str()) {
                    return Ok(dst);
                }
            }
            Err(_) => {
                clone_or_copy(src, &dst)?;
                set_tag(&dst, &tag);
                return Ok(dst);
            }
        }
    }
    Err(std::io::Error::other("too many files with that name"))
}

/// `materialize(sha, name?, project?)`: the asset as a human-named file in `<project>/.out/`.
pub fn materialize(sha: &str, name: Option<&str>, project: Option<&str>) -> Result<PathBuf, String> {
    let hex = parse_sha(sha).ok_or_else(|| format!("not a sha256 asset id: {sha}"))?;
    let host = host_file().ok_or("no fal-os host running (host.json missing)")?;
    let dirs = project_dirs(&host, project);
    let first = dirs.first().cloned().ok_or("host.json names no project")?;
    let (dir, src) = dirs
        .iter()
        .find_map(|d| find_asset(d, &hex).map(|p| (d.clone(), p)))
        .ok_or_else(|| format!("asset {} is not in the project store", &hex[..12]))?;
    // The copy goes next to the project that asked (else the one holding the bytes).
    let out = if project.is_some() { first } else { dir };
    materialize_file(&src, &hex, &out.join(".out"), name).map_err(|e| format!("materialize: {e}"))
}

/// `materialize(node, name?)` without a digest: the host resolves the node
/// (`POST /api/materialize`). A tiny blocking HTTP/1.1 client: loopback only, no deps.
pub fn materialize_via_host(body: &Json, project: Option<&str>) -> Result<PathBuf, String> {
    use std::io::{Read, Write};
    let host = host_file().ok_or("no fal-os host running")?;
    let port = host.get("port").and_then(Json::as_u64).ok_or("host.json has no port")?;
    let token = host.get("token").and_then(Json::as_str).ok_or("host.json has no token")?;
    let q = project.filter(|p| valid_project_id(p)).map(|p| format!("?project={p}")).unwrap_or_default();
    let payload = body.to_string();
    let req = format!(
        "POST /api/materialize{q} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port as u16)).map_err(|e| format!("host: {e}"))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(30))).ok();
    s.write_all(req.as_bytes()).map_err(|e| format!("host: {e}"))?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).map_err(|e| format!("host: {e}"))?;
    let text = String::from_utf8_lossy(&resp);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("host: bad response")?;
    let status = head.split_whitespace().nth(1).unwrap_or("");
    // A chunked body only when the host streams; ours is small, but tolerate it.
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        body.split("\r\n").skip(1).step_by(2).collect::<String>()
    } else {
        body.to_string()
    };
    if status != "200" {
        return Err(format!("host {status}: {}", body.trim()));
    }
    let v: Json = serde_json::from_str(body.trim()).map_err(|e| format!("host: {e}"))?;
    v.get("path").and_then(Json::as_str).map(PathBuf::from).ok_or_else(|| "host: no path".into())
}

/// Paths from the UI: absolute, existing files only.
pub fn checked_paths(paths: &[String]) -> Result<Vec<PathBuf>, String> {
    if paths.is_empty() {
        return Err("no files".into());
    }
    paths
        .iter()
        .map(|p| {
            let pb = PathBuf::from(p);
            if !pb.is_absolute() {
                return Err(format!("not an absolute path: {p}"));
            }
            if !pb.exists() {
                return Err(format!("no such file: {p}"));
            }
            Ok(pb)
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AppInfo {
    pub name: String,
    pub bundle_id: String,
    pub path: String,
    /// One of the pinned creative apps (listed first).
    pub pinned: bool,
    /// The app Finder would use.
    pub default: bool,
}

/// Creative apps listed first in "Open in…" when installed, even when they don't claim the
/// file type (Premiere doesn't claim .webp, Figma claims nothing). Several ids per app:
/// Adobe versions the bundle id by year; Resolve has a free "Lite" id.
pub const PINNED: &[(&str, &[&str])] = &[
    ("Premiere Pro", &["com.adobe.PremierePro.27", "com.adobe.PremierePro.26", "com.adobe.PremierePro.25", "com.adobe.PremierePro.24", "com.adobe.PremierePro.23"]),
    ("After Effects", &["com.adobe.AfterEffects"]),
    ("DaVinci Resolve", &["com.blackmagic-design.DaVinciResolve", "com.blackmagic-design.DaVinciResolveLite", "com.blackmagic-design.DaVinciResolveStudio"]),
    ("Final Cut Pro", &["com.apple.FinalCut", "com.apple.FinalCutTrial"]),
    ("Photoshop", &["com.adobe.Photoshop"]),
    ("Figma", &["com.figma.Desktop"]),
    ("Keynote", &["com.apple.iWork.Keynote"]),
    ("Preview", &["com.apple.Preview"]),
    ("QuickTime Player", &["com.apple.QuickTimePlayerX"]),
];

/// Pinned apps first (fixed order), then the apps Launch Services offers for the file, the
/// default app marked. Deduplicated by bundle id.
pub fn merge_apps(pinned: Vec<AppInfo>, offered: Vec<AppInfo>, default_path: Option<&str>) -> Vec<AppInfo> {
    let mut out: Vec<AppInfo> = Vec::new();
    for a in pinned.into_iter().chain(offered) {
        if out.iter().any(|b| (b.bundle_id == a.bundle_id && !a.bundle_id.is_empty()) || b.path == a.path) {
            continue;
        }
        out.push(a);
    }
    for a in &mut out {
        a.default = default_path == Some(a.path.as_str());
    }
    out
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::AnyThread;
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSDeviceRGBColorSpace, NSGraphicsContext, NSImage,
        NSPasteboard, NSPasteboardTypePNG, NSPasteboardWriting, NSWorkspace,
    };
    use objc2_foundation::{NSArray, NSBundle, NSData, NSDictionary, NSFileManager, NSPoint, NSRect, NSSize, NSString, NSURL};

    fn file_url(p: &Path) -> Retained<NSURL> {
        NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()))
    }

    pub fn copy_files(paths: &[PathBuf]) -> Result<(), String> {
        let urls: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> =
            paths.iter().map(|p| ProtocolObject::from_retained(file_url(p))).collect();
        let arr = NSArray::from_retained_slice(&urls);
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        if pb.writeObjects(&arr) { Ok(()) } else { Err("pasteboard refused the files".into()) }
    }

    fn load_image(p: &Path) -> Option<Retained<NSImage>> {
        NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&p.to_string_lossy()))
    }

    /// PNG bytes of `img` drawn into a `w`×`h` px bitmap.
    fn png_of(img: &NSImage, w: f64, h: f64) -> Option<Vec<u8>> {
        let (wi, hi) = (w.round().max(1.0) as isize, h.round().max(1.0) as isize);
        // SAFETY: null planes = AppKit allocates; RGBA 8-bit, non-planar, AppKit row bytes.
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                wi,
                hi,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                0,
                0,
            )
        }?;
        let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&ctx));
        img.drawInRect(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(wi as f64, hi as f64)));
        ctx.flushGraphics();
        NSGraphicsContext::restoreGraphicsState_class();
        // SAFETY: an empty properties dictionary is valid for PNG.
        let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
        Some(data.to_vec())
    }

    /// The image's own pixel size (largest bitmap rep), else its point size.
    fn pixel_size(img: &NSImage) -> NSSize {
        let mut best = img.size();
        for r in img.representations().iter() {
            let (w, h) = (r.pixelsWide() as f64, r.pixelsHigh() as f64);
            if w * h > best.width * best.height {
                best = NSSize::new(w, h);
            }
        }
        best
    }

    pub fn copy_image(path: &Path) -> Result<(), String> {
        let is_png = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("png"));
        let img = load_image(path).ok_or("not an image macOS can read")?;
        let png = if is_png {
            std::fs::read(path).map_err(|e| e.to_string())?
        } else {
            let s = pixel_size(&img);
            png_of(&img, s.width, s.height).ok_or("could not encode PNG")?
        };
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        // NSImage writes TIFF (what older apps read); the PNG goes on the same item.
        let obj: Retained<ProtocolObject<dyn NSPasteboardWriting>> = ProtocolObject::from_retained(img);
        pb.writeObjects(&NSArray::from_retained_slice(&[obj]));
        // SAFETY: extern static provided by AppKit.
        let ty = unsafe { NSPasteboardTypePNG };
        if pb.setData_forType(Some(&NSData::from_vec(png)), ty) { Ok(()) } else { Err("pasteboard refused the image".into()) }
    }

    /// Drag preview: the image itself (images) or the Finder icon (anything else), at most
    /// `max` px on the long side; several files show the multi-file icon.
    pub fn drag_icon(paths: &[PathBuf], max: f64) -> Option<Vec<u8>> {
        let first = paths.first()?;
        let ws = NSWorkspace::sharedWorkspace();
        let img = if paths.len() == 1 {
            load_image(first).filter(|i| {
                let s = i.size();
                s.width > 0.0 && s.height > 0.0
            })
        } else {
            None
        };
        let is_image = img.is_some();
        let img = match img {
            Some(i) => i,
            None if paths.len() > 1 => {
                let names: Vec<Retained<NSString>> = paths.iter().map(|p| NSString::from_str(&p.to_string_lossy())).collect();
                ws.iconForFiles(&NSArray::from_retained_slice(&names)).unwrap_or_else(|| ws.iconForFile(&NSString::from_str(&first.to_string_lossy())))
            }
            None => ws.iconForFile(&NSString::from_str(&first.to_string_lossy())),
        };
        let s = img.size();
        let k = (max / s.width.max(s.height).max(1.0)).clamp(0.01, 1.0);
        let (w, h) = if is_image { (s.width * k, s.height * k) } else { (max * 0.75, max * 0.75) };
        png_of(&img, w, h)
    }

    fn app_info(url: &NSURL, pinned: bool) -> Option<AppInfo> {
        let path = url.path()?.to_string();
        let bundle_id = NSBundle::bundleWithURL(url).and_then(|b| b.bundleIdentifier()).map(|s| s.to_string()).unwrap_or_default();
        let name = NSFileManager::defaultManager().displayNameAtPath(&NSString::from_str(&path)).to_string();
        let name = name.strip_suffix(".app").unwrap_or(&name).to_string();
        Some(AppInfo { name, bundle_id, path, pinned, default: false })
    }

    pub fn open_with_list(path: &Path) -> Vec<AppInfo> {
        let ws = NSWorkspace::sharedWorkspace();
        let url = file_url(path);
        let mut pinned = Vec::new();
        for (_, ids) in PINNED {
            if let Some(a) = ids.iter().find_map(|id| ws.URLForApplicationWithBundleIdentifier(&NSString::from_str(id)))
                && let Some(info) = app_info(&a, true)
            {
                pinned.push(info);
            }
        }
        let offered: Vec<AppInfo> = ws.URLsForApplicationsToOpenURL(&url).iter().filter_map(|u| app_info(&u, false)).collect();
        let default = ws.URLForApplicationToOpenURL(&url).and_then(|u| u.path()).map(|p| p.to_string());
        merge_apps(pinned, offered, default.as_deref())
    }

    pub fn app_for_bundle(id: &str) -> Option<String> {
        NSWorkspace::sharedWorkspace().URLForApplicationWithBundleIdentifier(&NSString::from_str(id)).and_then(|u| u.path()).map(|p| p.to_string())
    }
}

#[cfg(target_os = "macos")]
pub use mac::{copy_files, copy_image, drag_icon, open_with_list};

#[cfg(not(target_os = "macos"))]
pub fn copy_files(_paths: &[PathBuf]) -> Result<(), String> {
    Err("copy files is macOS-only for now".into())
}
#[cfg(not(target_os = "macos"))]
pub fn copy_image(_path: &Path) -> Result<(), String> {
    Err("copy image is macOS-only for now".into())
}
#[cfg(not(target_os = "macos"))]
pub fn drag_icon(_paths: &[PathBuf], _max: f64) -> Option<Vec<u8>> {
    None
}
#[cfg(not(target_os = "macos"))]
pub fn open_with_list(_path: &Path) -> Vec<AppInfo> {
    vec![]
}

/// Opens `path` with an app given by path (`/Applications/X.app`) or bundle id.
pub fn open_with(path: &Path, app: &str) -> Result<(), String> {
    let mut cmd = std::process::Command::new("/usr/bin/open");
    if app.ends_with(".app") || app.starts_with('/') {
        if !Path::new(app).exists() {
            return Err(format!("no such app: {app}"));
        }
        cmd.arg("-a").arg(app);
    } else {
        #[cfg(target_os = "macos")]
        if mac::app_for_bundle(app).is_none() {
            return Err(format!("{app} is not installed"));
        }
        cmd.arg("-b").arg(app);
    }
    let st = cmd.arg(path).status().map_err(|e| e.to_string())?;
    if st.success() { Ok(()) } else { Err(format!("open failed ({st})")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("falos-out-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn rand_suffix() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
    }

    #[test]
    fn stems_are_safe() {
        assert_eq!(safe_stem("hero / v2: final"), "hero - v2- final");
        assert_eq!(safe_stem("..hidden"), "hidden");
        assert_eq!(safe_stem("  "), "");
        assert_eq!(safe_stem(&"x".repeat(300)).len(), 120);
    }

    #[test]
    fn materializes_clones_reuses_and_never_hardlinks() {
        let d = tmp();
        let assets = d.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        let hex = "ab".repeat(32);
        let src = assets.join(format!("{hex}.png"));
        std::fs::write(&src, b"pngbytes").unwrap();
        let out = d.join(".out");
        let a = materialize_file(&src, &hex, &out, Some("Hero shot.png")).unwrap();
        assert_eq!(a, out.join("Hero shot.png"));
        assert_eq!(std::fs::read(&a).unwrap(), b"pngbytes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let (ms, ma) = (std::fs::metadata(&src).unwrap(), std::fs::metadata(&a).unwrap());
            assert_ne!(ms.ino(), ma.ino(), "must not be a hardlink");
            assert_eq!(ms.nlink(), 1);
        }
        // Same bytes again: the same file.
        assert_eq!(materialize_file(&src, &hex, &out, Some("Hero shot")).unwrap(), a);
        // The other app changed the copy: a new name, the store untouched.
        std::fs::write(&a, b"edited by photoshop").unwrap();
        let b = materialize_file(&src, &hex, &out, Some("Hero shot")).unwrap();
        assert_eq!(b, out.join("Hero shot (1).png"));
        assert_eq!(std::fs::read(&src).unwrap(), b"pngbytes");
        // No name: the digest prefix.
        let c = materialize_file(&src, &hex, &out, None).unwrap();
        assert_eq!(c.file_name().unwrap().to_str().unwrap(), format!("{}.png", &hex[..12]));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn finds_projects_and_assets() {
        let d = tmp();
        let ws = d.join("ws");
        for p in ["one", "two"] {
            std::fs::create_dir_all(ws.join(p).join("assets")).unwrap();
            std::fs::write(ws.join(p).join("project.json"), "{}").unwrap();
        }
        let hex = "cd".repeat(32);
        std::fs::write(ws.join("two/assets").join(format!("{hex}.mp4")), b"v").unwrap();
        let host = serde_json::json!({"workspace": ws, "project": ws.join("one")});
        let dirs = project_dirs(&host, Some("two"));
        assert_eq!(dirs[0], ws.join("two"));
        assert_eq!(dirs[1], ws.join("one"));
        assert_eq!(dirs.len(), 2);
        assert!(project_dirs(&host, Some("../etc")).iter().all(|p| p.starts_with(&ws)));
        assert!(find_asset(&ws.join("two"), &hex).is_some());
        assert!(find_asset(&ws.join("one"), &hex).is_none());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn merges_pinned_first() {
        let app = |n: &str, id: &str, pinned| AppInfo { name: n.into(), bundle_id: id.into(), path: format!("/Applications/{n}.app"), pinned, default: false };
        let m = merge_apps(
            vec![app("Preview", "com.apple.Preview", true)],
            vec![app("Preview", "com.apple.Preview", false), app("Pixelmator", "com.pixelmator", false)],
            Some("/Applications/Pixelmator.app"),
        );
        assert_eq!(m.len(), 2);
        assert!(m[0].pinned && !m[0].default);
        assert!(m[1].default);
    }

    #[test]
    fn checks_paths() {
        assert!(checked_paths(&[]).is_err());
        assert!(checked_paths(&["relative.png".into()]).is_err());
        assert!(checked_paths(&["/definitely/not/here.png".into()]).is_err());
        assert!(checked_paths(&["/".into()]).is_ok());
    }

    /// Lists apps for a real file (Launch Services; no UI). `cargo test -- --ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn lists_open_with_apps() {
        let d = tmp();
        let f = d.join("x.png");
        std::fs::write(&f, b"\x89PNG\r\n\x1a\n").unwrap();
        let apps = open_with_list(&f);
        for a in &apps {
            println!("{:>5} {:>5} {:<28} {:<40} {}", a.pinned, a.default, a.name, a.bundle_id, a.path);
        }
        assert!(apps.iter().any(|a| a.bundle_id == "com.apple.Preview"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// Against a running host (`FALOS_HOME` of a temp host, `FALOS_MAT_SHA` an asset in its
    /// project `FALOS_MAT_PROJECT`): materialize by digest, and by node via the host.
    #[test]
    #[ignore]
    fn live_materialize() {
        let Ok(sha) = std::env::var("FALOS_MAT_SHA") else { return };
        let project = std::env::var("FALOS_MAT_PROJECT").ok();
        let p = materialize(&sha, Some("Live clone"), project.as_deref()).unwrap();
        println!("by sha: {}", p.display());
        assert!(p.ends_with(".out/Live clone.png"));
        if let Ok(node) = std::env::var("FALOS_MAT_NODE") {
            let q = materialize_via_host(&serde_json::json!({"node": node, "name": "Via host"}), project.as_deref()).unwrap();
            println!("by node: {}", q.display());
            assert!(q.ends_with(".out/Via host.png"));
        }
    }

    /// Writes to the user's general pasteboard: run only on purpose, with
    /// `FALOS_PB_TEST=<dir>` (files are created there and left for inspection), e.g.
    /// `osascript -e 'clipboard info'` afterwards shows «class furl».
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn pasteboard_writes() {
        let Some(dir) = std::env::var_os("FALOS_PB_TEST").map(PathBuf::from) else { return };
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        match std::env::var("FALOS_PB_TEST_KIND").as_deref() {
            Ok("image") => {
                let icon = drag_icon(std::slice::from_ref(&a), 64.0).unwrap();
                let png = dir.join("icon.png");
                std::fs::write(&png, icon).unwrap();
                copy_image(&png).unwrap();
            }
            _ => copy_files(&[a, b]).unwrap(),
        }
        let pb = objc2_app_kit::NSPasteboard::generalPasteboard();
        let types: Vec<String> = pb.types().map(|t| t.iter().map(|s| s.to_string()).collect()).unwrap_or_default();
        println!("types after write: {types:?} items={}", pb.pasteboardItems().map_or(0, |i| i.count()));
    }

    /// Drag preview PNG from a real image and from a non-image (Finder icon).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn makes_drag_icons() {
        let d = tmp();
        let f = d.join("doc.txt");
        std::fs::write(&f, b"hello").unwrap();
        let png = drag_icon(std::slice::from_ref(&f), 96.0).expect("icon");
        assert!(png.starts_with(b"\x89PNG"));
        let two = drag_icon(&[f.clone(), f.clone()], 96.0).expect("icon");
        assert!(two.starts_with(b"\x89PNG"));
        std::fs::remove_dir_all(&d).ok();
    }
}
