//! Native clipboard reads for ⌘V on the canvas. WKWebView's paste event and async clipboard
//! don't reliably expose image data or Finder-copied files (and `navigator.clipboard.read`
//! shows a "Paste" bubble), so the web side asks here first: files copied in Finder come back
//! as their paths, raw image data (screenshots, "Copy Image") is written to a temp PNG. The
//! web then fetches the bytes with `read_clip_file`, which only serves paths handed out here.
//!
//! A file pasted from Desktop, Documents, Downloads, iCloud Drive or a removable volume is the
//! one case where macOS asks for folder access (a paste, unlike a drop or the open panel, carries
//! no implied consent). So listing never touches the files (no stat), each file names its
//! protected `folder`, and the web says why before `read_clip_file` (off the main thread) reads it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

static SERVED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

#[derive(serde::Serialize)]
pub struct ClipFile {
    path: String,
    name: String,
    /// The macOS-protected folder it's in (`Desktop`, `iCloud Drive`, …), if any.
    folder: Option<&'static str>,
}

/// The privacy-protected location `p` is in: reading it makes macOS ask once per folder.
pub fn protected_folder(p: &std::path::Path, home: &std::path::Path) -> Option<&'static str> {
    if p.starts_with("/Volumes") {
        return Some("a removable or network volume");
    }
    let rel = p.strip_prefix(home).ok()?;
    [
        ("Desktop", "Desktop"),
        ("Documents", "Documents"),
        ("Downloads", "Downloads"),
        ("Library/Mobile Documents", "iCloud Drive"),
        ("Library/CloudStorage", "cloud storage"),
        ("Pictures/Photos Library.photoslibrary", "Photos"),
    ]
    .into_iter()
    .find(|(dir, _)| rel.starts_with(dir))
    .map(|(_, name)| name)
}

fn allow(p: &PathBuf) {
    SERVED.lock().unwrap().get_or_insert_with(HashSet::new).insert(p.clone());
}

/// Files on the clipboard: Finder-copied files first, else image data as a temp PNG. Paths only:
/// nothing here touches the files (a folder fails later, in `read_clip_file`).
#[tauri::command]
pub fn clipboard_files() -> Result<Vec<ClipFile>, String> {
    let paths = imp::read_files()?;
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    Ok(paths
        .into_iter()
        .map(|p| {
            allow(&p);
            ClipFile {
                name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                folder: protected_folder(&p, &home),
                path: p.to_string_lossy().into_owned(),
            }
        })
        .collect())
}

/// Bytes of a file previously returned by `clipboard_files`. Off the main thread: a macOS
/// folder-access prompt blocks the read until the user answers.
#[tauri::command(async)]
pub fn read_clip_file(path: String) -> Result<tauri::ipc::Response, String> {
    let p = PathBuf::from(&path);
    if !SERVED.lock().unwrap().as_ref().is_some_and(|s| s.contains(&p)) {
        return Err("not a clipboard file".into());
    }
    std::fs::read(&p).map(tauri::ipc::Response::new).map_err(|e| e.to_string())
}

/// Plain text on the clipboard, without WKWebView's paste-permission bubble.
#[tauri::command]
pub fn clipboard_text() -> Result<String, String> {
    imp::read_text()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypePNG,
        NSPasteboardTypeString, NSPasteboardTypeTIFF,
    };
    use objc2_foundation::{NSDictionary, NSString, NSURL};

    pub fn read_files() -> Result<Vec<PathBuf>, String> {
        let pb = NSPasteboard::generalPasteboard();
        let mut out = Vec::new();
        // SAFETY: extern statics provided by AppKit.
        let (t_url, t_png, t_tiff) = unsafe { (NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeTIFF) };
        if let Some(items) = pb.pasteboardItems() {
            for item in items.iter() {
                if let Some(s) = item.stringForType(t_url) {
                    if let Some(p) = NSURL::URLWithString(&s).and_then(|u| u.path()) {
                        out.push(PathBuf::from(p.to_string()));
                    }
                }
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
        let png = if let Some(d) = pb.dataForType(t_png) {
            Some(d.to_vec())
        } else if let Some(d) = pb.dataForType(t_tiff) {
            NSBitmapImageRep::imageRepWithData(&d).and_then(|rep| {
                // SAFETY: an empty properties dictionary is valid for PNG.
                unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }
            }).map(|d| d.to_vec())
        } else {
            None
        };
        if let Some(bytes) = png {
            let dir = std::env::temp_dir().join("falos-clip");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
            let p = dir.join(format!("pasted-{stamp}.png"));
            std::fs::write(&p, bytes).map_err(|e| e.to_string())?;
            out.push(p);
        }
        Ok(out)
    }

    pub fn read_text() -> Result<String, String> {
        let pb = NSPasteboard::generalPasteboard();
        // SAFETY: extern static provided by AppKit.
        Ok(pb.stringForType(unsafe { NSPasteboardTypeString }).map(|s: objc2::rc::Retained<NSString>| s.to_string()).unwrap_or_default())
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    pub fn read_files() -> Result<Vec<PathBuf>, String> {
        Ok(Vec::new())
    }
    pub fn read_text() -> Result<String, String> {
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::protected_folder;
    use std::path::Path;

    #[test]
    fn names_protected_folders() {
        let h = Path::new("/Users/a");
        assert_eq!(protected_folder(Path::new("/Users/a/Desktop/x.png"), h), Some("Desktop"));
        assert_eq!(protected_folder(Path::new("/Users/a/Library/Mobile Documents/com~apple~CloudDocs/x.png"), h), Some("iCloud Drive"));
        assert_eq!(protected_folder(Path::new("/Volumes/SD/x.mov"), h), Some("a removable or network volume"));
        assert_eq!(protected_folder(Path::new("/Users/a/fal-os-projects/p/x.png"), h), None);
        assert_eq!(protected_folder(Path::new("/var/folders/x/falos-clip/pasted-1.png"), h), None);
        assert_eq!(protected_folder(Path::new("/Users/a/DesktopX/x.png"), h), None);
    }
}
