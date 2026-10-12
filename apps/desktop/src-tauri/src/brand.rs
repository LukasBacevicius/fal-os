//! Dock icon that follows the working state: the LED fal mark lit while agents work, dimmed
//! when idle (assets/brand/tools/generate.py renders `icons/dock-{lit,dim}.png`). The bundle's
//! static icon (icon.icns) is the lit one; the swap only lasts while the app runs.

#[cfg(target_os = "macos")]
static LIT: &[u8] = include_bytes!("../icons/dock-lit.png");
#[cfg(target_os = "macos")]
static DIM: &[u8] = include_bytes!("../icons/dock-dim.png");

/// Sets `NSApplication.applicationIconImage` (main thread only).
#[cfg(target_os = "macos")]
pub fn set_dock_icon(working: bool) {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(mtm) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(if working { LIT } else { DIM });
    if let Some(img) = NSImage::initWithData(NSImage::alloc(), &data) {
        // SAFETY: on the main thread (mtm) with a valid, retained image.
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&img)) };
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon(_working: bool) {}
