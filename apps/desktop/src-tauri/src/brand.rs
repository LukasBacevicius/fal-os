//! Dock icon. The mark is always lit, so the app keeps the bundle icon (icon.icns), which macOS
//! masks to the system's rounded tile. Swapping `applicationIconImage` at runtime would bypass
//! that mask and show a differently rounded tile while the app runs, so this is a no-op now.

pub fn set_dock_icon(_working: bool) {}
