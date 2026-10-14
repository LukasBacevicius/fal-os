//! Native clipboard reads for ⌘V on the canvas. WKWebView's paste event and async clipboard
//! don't reliably expose image data or Finder-copied files (and `navigator.clipboard.read`
//! shows a "Paste" bubble), so the web side asks here first: files copied in Finder come back
//! as their paths, raw image data (screenshots, "Copy Image") is written to a temp PNG. The
//! web then fetches the bytes with `read_clip_file`, which only serves paths handed out here.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

static SERVED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

#[derive(serde::Serialize)]
pub struct ClipFile {
    path: String,
    name: String,
}

fn allow(p: &PathBuf) {
    SERVED.lock().unwrap().get_or_insert_with(HashSet::new).insert(p.clone());
}

/// Files on the clipboard: Finder-copied files first, else image data as a temp PNG.
#[tauri::command]
pub fn clipboard_files() -> Result<Vec<ClipFile>, String> {
    let paths = imp::read_files()?;
    Ok(paths
        .into_iter()
        .filter(|p| p.is_file())
        .map(|p| {
            allow(&p);
            ClipFile { name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), path: p.to_string_lossy().into_owned() }
        })
        .collect())
}

/// Bytes of a file previously returned by `clipboard_files`.
#[tauri::command]
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
