//! Local crash diagnostics (no telemetry): a panic hook that writes
//! `$FALOS_HOME/logs/crash-<ms>-<component>.txt` (message, location, backtrace, version, the
//! tail of `host.log`), and a one-time notice on the next start, "fal OS stopped unexpectedly
//! last time · [copy report]", for reports nobody has seen yet. The desktop shell writes the
//! same kind of report for its own panics and for a host that died (see its `sidecar.rs`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value as Json, json};

/// `$FALOS_HOME/logs`.
pub fn logs_dir() -> PathBuf {
    crate::server::home_dir().join("logs")
}

/// Marker: crash reports older than this file's mtime were already announced.
const SEEN: &str = ".crash-seen";

/// The last `n` lines of a text file (empty when missing).
pub fn tail(path: &Path, n: usize) -> String {
    let Ok(bytes) = std::fs::read(path) else { return String::new() };
    // Only the end of a big log matters.
    let start = bytes.len().saturating_sub(256 * 1024);
    let text = String::from_utf8_lossy(&bytes[start..]);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Write a crash report; returns its path.
pub fn write_report(component: &str, body: &str) -> std::io::Result<PathBuf> {
    let dir = logs_dir();
    std::fs::create_dir_all(&dir)?;
    let ms = crate::host::now_ms();
    let path = dir.join(format!("crash-{ms}-{component}.txt"));
    let mut text = format!(
        "fal OS crash report\ncomponent: {component}\nversion: {}\ntime: {}\npid: {}\nos: {} {}\n\n{body}\n",
        env!("CARGO_PKG_VERSION"),
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z"),
        std::process::id(),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    let log = tail(&dir.join("host.log"), 200);
    if !log.is_empty() {
        text.push_str("\n--- last lines of host.log ---\n");
        text.push_str(&log);
        text.push('\n');
    }
    std::fs::write(&path, text)?;
    Ok(path)
}

/// Install a panic hook that writes a crash report, then runs the default hook (stderr).
/// `FALOS_TEST_PANIC=1` panics right away on purpose (to check the whole path).
pub fn install_panic_hook(component: &'static str) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".into());
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        let bt = std::backtrace::Backtrace::force_capture();
        let body = format!("panic: {msg}\nat: {loc}\nthread: {thread}\n\nbacktrace:\n{bt}");
        if let Ok(p) = write_report(component, &body) {
            eprintln!("fal OS: crash report written to {}", p.display());
        }
        default(info);
    }));
}

static CRASH: Mutex<Option<Json>> = Mutex::new(None);

/// Crash reports written since the last announced one (newest first).
pub fn unseen_reports() -> Vec<PathBuf> {
    let dir = logs_dir();
    let seen = std::fs::metadata(dir.join(SEEN)).and_then(|m| m.modified()).ok();
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with("crash-") && n.ends_with(".txt")))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(t, _)| seen.is_none_or(|s| *t > s))
        .collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.0));
    v.into_iter().map(|(_, p)| p).collect()
}

/// At start: turn unseen crash reports into the one-time notice and mark them seen.
pub fn load_crash_notice() {
    let reports = unseen_reports();
    let Some(newest) = reports.first() else { return };
    let mut copy = std::fs::read_to_string(newest).unwrap_or_default();
    if copy.len() > 30_000 {
        let mut cut = 30_000;
        while !copy.is_char_boundary(cut) {
            cut -= 1;
        }
        copy.truncate(cut);
        copy.push_str("\n… (truncated)");
    }
    copy.push_str(&format!("\n\nreport file: {}\n", newest.display()));
    let more = if reports.len() > 1 { format!(" ({} reports)", reports.len()) } else { String::new() };
    // The desktop app restarted a host that died: this is that restart, not a new launch.
    let restarted = newest.file_name().is_some_and(|n| n.to_string_lossy().ends_with("-host-exit.txt"))
        && std::env::var_os("FALOS_EXIT_WITH_PARENT").is_some();
    let text = if restarted {
        format!("the fal OS host stopped unexpectedly and was restarted{more}")
    } else {
        format!("fal OS stopped unexpectedly last time{more}")
    };
    *CRASH.lock().unwrap() = Some(json!({
        "key": "crash",
        "level": "error",
        "text": text,
        "actions": [{"id": "copy", "label": "copy report"}, {"id": "dismiss", "label": "dismiss"}],
        "copy": copy,
    }));
    let dir = logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(SEEN), b"");
}

/// Notices that belong to the whole app, not a project (the crash notice).
pub fn global_notices() -> Vec<Json> {
    CRASH.lock().unwrap().iter().cloned().collect()
}

pub fn dismiss(key: &str) -> bool {
    let mut c = CRASH.lock().unwrap();
    if c.as_ref().is_some_and(|n| n["key"] == key) {
        *c = None;
        return true;
    }
    false
}
