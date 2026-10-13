//! The menu bar item: the fal LED mark as a template image (one more LED lit in the hole while
//! agents work), a plain-text menu built from the host's `GET /api/status` (polled once a
//! second), and the quiet notifications (a run finished, or a run needs the user) while the
//! window is hidden or unfocused.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value as Json, json};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt as _;

static IDLE: &[u8] = include_bytes!("../icons/tray.png");
static WORKING: &[u8] = include_bytes!("../icons/tray-working.png");

const POLL: Duration = Duration::from_secs(1);
/// Menu labels are cut here (the menu bar menu shouldn't grow wider than a third of a screen).
const LABEL: usize = 72;
/// Chat titles are cut shorter, so the status after them stays readable.
const TITLE: usize = 32;

// ---- host status ------------------------------------------------------------------------------

/// `GET <path>` on the local host (`host.json` port + token), JSON body.
pub fn host_get(path: &str) -> Result<Json, String> {
    use std::io::{Read, Write};
    let host = crate::out::host_file().ok_or("no fal-os host running")?;
    let port = host.get("port").and_then(Json::as_u64).ok_or("host.json has no port")?;
    let token = host.get("token").and_then(Json::as_str).ok_or("host.json has no token")?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port as u16));
    let mut s = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&resp);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("bad response")?;
    if head.split_whitespace().nth(1) != Some("200") {
        return Err(format!("host: {}", head.lines().next().unwrap_or_default()));
    }
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        body.split("\r\n").skip(1).step_by(2).collect::<String>()
    } else {
        body.to_string()
    };
    serde_json::from_str(body.trim()).map_err(|e| e.to_string())
}

pub fn status() -> Option<Json> {
    host_get("/api/status").ok()
}

/// "2 agent runs are active" (+ generations), or None when nothing would be cut off.
pub fn active_summary(s: &Json) -> Option<String> {
    let runs = s["runs"].as_array().map_or(0, Vec::len);
    let gens = s["generating"].as_u64().unwrap_or(0) + s["queued"].as_u64().unwrap_or(0);
    let r = match runs {
        0 => None,
        1 => Some("1 agent run is active".to_string()),
        n => Some(format!("{n} agent runs are active")),
    };
    let g = (gens > 0).then(|| if gens == 1 { "1 generation".to_string() } else { format!("{gens} generations") });
    match (r, g) {
        (Some(r), Some(g)) => Some(format!("{r} and {g} in flight")),
        (Some(r), None) => Some(r),
        (None, Some(g)) => Some(format!("{g} {} in flight", if gens == 1 { "is" } else { "are" })),
        (None, None) => None,
    }
}

// ---- menu model ------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Item { id: String, text: String, enabled: bool },
    Check { id: String, text: String, checked: bool },
    Sep,
}

impl Row {
    fn item(id: impl Into<String>, text: impl Into<String>) -> Self {
        Row::Item { id: id.into(), text: text.into(), enabled: true }
    }
    fn info(text: impl Into<String>) -> Self {
        Row::Item { id: String::new(), text: text.into(), enabled: false }
    }
    /// Rows with the same shape can be updated in place (text / checked) instead of rebuilt.
    fn shape(&self) -> (u8, &str, bool) {
        match self {
            Row::Item { id, enabled, .. } => (0, id, *enabled),
            Row::Check { id, .. } => (1, id, true),
            Row::Sep => (2, "", false),
        }
    }
}

fn clip(s: &str, n: usize) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > n { format!("{}…", s.chars().take(n - 1).collect::<String>()) } else { s }
}

/// `run:<project>/<chat>`: ids the menu handler opens.
fn target(v: &Json) -> String {
    format!("{}/{}", v["project"].as_str().unwrap_or_default(), v["chat"].as_str().unwrap_or_default())
}

pub fn rows(status: Option<&Json>, login: bool) -> Vec<Row> {
    let mut out = Vec::new();
    match status {
        None => out.push(Row::info("host starting…")),
        Some(s) => {
            for r in s["runs"].as_array().into_iter().flatten() {
                let what = match r["question"]["prompt"].as_str() {
                    Some(p) if !p.is_empty() => format!("needs you: {p}"),
                    Some(_) => "needs you".into(),
                    None => r["step"].as_str().filter(|t| !t.is_empty()).unwrap_or("working").to_string(),
                };
                let head = format!("● {} · {}", clip(r["project_name"].as_str().unwrap_or_default(), 24), clip(r["title"].as_str().unwrap_or_default(), TITLE));
                out.push(Row::item(format!("run:{}", target(r)), clip(&format!("{head} — {what}"), LABEL)));
            }
            let (g, q) = (s["generating"].as_u64().unwrap_or(0), s["queued"].as_u64().unwrap_or(0));
            if g + q > 0 {
                out.push(Row::info(if q > 0 { format!("{g} generating · {q} queued") } else { format!("{g} generating") }));
            }
            let recent = s["recent"].as_array().filter(|r| !r.is_empty());
            if let Some(recent) = recent {
                if !out.is_empty() {
                    out.push(Row::Sep);
                }
                for r in recent {
                    let failed = r["state"] == "error";
                    let text = format!(
                        "{} {} · {} — {}",
                        if failed { "×" } else { "✓" },
                        clip(r["project_name"].as_str().unwrap_or_default(), 24),
                        clip(r["title"].as_str().unwrap_or_default(), TITLE),
                        if failed { "failed · show" } else { "show" }
                    );
                    out.push(Row::item(format!("show:{}", target(r)), text));
                }
            }
            if out.is_empty() {
                out.push(Row::info("nothing running"));
            }
        }
    }
    out.push(Row::Sep);
    out.push(Row::item("open", "open fal-os"));
    out.push(Row::item("new", "new project"));
    out.push(Row::Check { id: "login".into(), text: "open at login".into(), checked: login });
    out.push(Row::item("quit", "quit"));
    out
}

pub fn working(status: Option<&Json>) -> bool {
    status.is_some_and(|s| {
        s["runs"].as_array().is_some_and(|r| !r.is_empty())
            || s["generating"].as_u64().unwrap_or(0) + s["queued"].as_u64().unwrap_or(0) > 0
    })
}

// ---- notifications ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub struct Notice {
    pub project: String,
    pub chat: String,
    pub title: String,
    pub body: String,
}

/// What was already announced: the first status is the baseline (nothing old is announced).
#[derive(Default)]
pub struct Seen {
    started: bool,
    questions: HashSet<String>,
    ended: i64,
}

impl Seen {
    /// New "needs you" questions and runs that ended since the last status.
    pub fn diff(&mut self, s: &Json) -> Vec<Notice> {
        let mut out = Vec::new();
        let first = !self.started;
        self.started = true;
        let notice = |v: &Json, body: String| Notice {
            project: v["project"].as_str().unwrap_or_default().into(),
            chat: v["chat"].as_str().unwrap_or_default().into(),
            title: clip(v["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("fal-os"), 60),
            body,
        };
        let mut live = HashSet::new();
        for r in s["runs"].as_array().into_iter().flatten() {
            let Some(q) = r.get("question").filter(|q| q.is_object()) else { continue };
            let key = format!("{}#{}", target(r), q["id"].as_str().unwrap_or_default());
            if !first && !self.questions.contains(&key) {
                let p = q["prompt"].as_str().filter(|p| !p.is_empty()).unwrap_or("waiting for your answer");
                out.push(notice(r, format!("{} · needs you: {}", r["project_name"].as_str().unwrap_or_default(), clip(p, 120))));
            }
            live.insert(key);
        }
        self.questions = live;
        let mut newest = self.ended;
        for r in s["recent"].as_array().into_iter().flatten() {
            let ended = r["ended"].as_i64().unwrap_or(0);
            newest = newest.max(ended);
            if !first && ended > self.ended {
                let what = if r["state"] == "error" { "stopped with an error" } else { "done" };
                out.push(notice(r, format!("{} · {what}", r["project_name"].as_str().unwrap_or_default())));
            }
        }
        self.ended = newest;
        out
    }
}

// ---- the tray itself -------------------------------------------------------------------------

struct Live {
    tray: TrayIcon,
    rows: Vec<Row>,
    /// Handles of the current menu's rows (None for separators), for in-place updates.
    items: Vec<Option<Handle>>,
    working: bool,
}

enum Handle {
    Item(MenuItem<Wry>),
    Check(CheckMenuItem<Wry>),
}

fn build_menu(app: &AppHandle, rows: &[Row]) -> tauri::Result<(Menu<Wry>, Vec<Option<Handle>>)> {
    let menu = Menu::new(app)?;
    let mut handles = Vec::new();
    for r in rows {
        match r {
            Row::Item { id, text, enabled } => {
                let i = if id.is_empty() {
                    MenuItem::new(app, text, *enabled, None::<&str>)?
                } else {
                    MenuItem::with_id(app, id, text, *enabled, None::<&str>)?
                };
                menu.append(&i)?;
                handles.push(Some(Handle::Item(i)));
            }
            Row::Check { id, text, checked } => {
                let i = CheckMenuItem::with_id(app, id, text, true, *checked, None::<&str>)?;
                menu.append(&i)?;
                handles.push(Some(Handle::Check(i)));
            }
            Row::Sep => {
                let s = PredefinedMenuItem::separator(app)?;
                menu.append(&s as &dyn IsMenuItem<Wry>)?;
                handles.push(None);
            }
        }
    }
    Ok((menu, handles))
}

fn icon(working: bool) -> Image<'static> {
    Image::from_bytes(if working { WORKING } else { IDLE }).expect("tray icon png")
}

/// Create the menu bar item and start the status poller.
pub fn start(app: &AppHandle) -> tauri::Result<()> {
    let rows0 = rows(None, login_enabled(app));
    let (menu, items) = build_menu(app, &rows0)?;
    let tray = TrayIconBuilder::with_id("falos")
        .icon(icon(false))
        .icon_as_template(true)
        .tooltip("fal-os")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .build(app)?;
    let live = Mutex::new(Live { tray, rows: rows0, items, working: false });
    let app = app.clone();
    std::thread::spawn(move || {
        let mut seen = Seen::default();
        loop {
            let s = status();
            if let Some(s) = &s {
                let notes = seen.diff(s);
                if !notes.is_empty() && !window_in_front(&app) {
                    for n in notes {
                        crate::notify::post(&n.project, &n.chat, &n.title, &n.body);
                    }
                }
            }
            let next = rows(s.as_ref(), login_enabled(&app));
            let work = working(s.as_ref());
            let app2 = app.clone();
            let mut l = live.lock().unwrap();
            if work != l.working {
                l.working = work;
                let _ = l.tray.set_icon_with_as_template(Some(icon(work)), true);
            }
            if next != l.rows {
                let same_shape = next.len() == l.rows.len() && next.iter().zip(&l.rows).all(|(a, b)| a.shape() == b.shape());
                if same_shape {
                    for (r, h) in next.iter().zip(&l.items) {
                        match (r, h) {
                            (Row::Item { text, .. }, Some(Handle::Item(i))) => {
                                let _ = i.set_text(text);
                            }
                            (Row::Check { text, checked, .. }, Some(Handle::Check(i))) => {
                                let _ = i.set_text(text);
                                let _ = i.set_checked(*checked);
                            }
                            _ => {}
                        }
                    }
                } else if let Ok((menu, items)) = build_menu(&app2, &next) {
                    let _ = l.tray.set_menu(Some(menu));
                    l.items = items;
                }
                l.rows = next;
            }
            drop(l);
            std::thread::sleep(POLL);
        }
    });
    Ok(())
}

fn login_enabled(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

fn window_in_front(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .is_some_and(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
}

/// Tray menu clicks (ids from [`rows`]).
pub fn on_menu(app: &AppHandle, id: &str) {
    if let Some(t) = id.strip_prefix("run:").or_else(|| id.strip_prefix("show:"))
        && let Some((project, chat)) = t.split_once('/')
    {
        crate::open_chat(app, project, chat);
        return;
    }
    match id {
        "open" => crate::show_main(app),
        "new" => {
            crate::show_main(app);
            crate::web_event(app, "falos:new-project", &json!({}));
        }
        "login" => {
            let a = app.autolaunch();
            let _ = if a.is_enabled().unwrap_or(false) { a.disable() } else { a.enable() };
        }
        "quit" => crate::request_quit(app),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Json {
        json!({
            "runs": [{"project": "p", "project_name": "Launch", "chat": "c1", "title": "Hero shots", "step": "generating 4 hero shots", "question": null}],
            "generating": 12, "queued": 3,
            "recent": [{"project": "p", "project_name": "Launch", "chat": "c0", "title": "Logo", "state": "idle", "ended": 100}],
        })
    }

    #[test]
    fn menu_rows() {
        let r = rows(Some(&status()), true);
        let texts: Vec<String> = r
            .iter()
            .map(|r| match r {
                Row::Item { text, .. } | Row::Check { text, .. } => text.clone(),
                Row::Sep => "--".into(),
            })
            .collect();
        assert_eq!(
            texts,
            [
                "● Launch · Hero shots — generating 4 hero shots",
                "12 generating · 3 queued",
                "--",
                "✓ Launch · Logo — show",
                "--",
                "open fal-os",
                "new project",
                "open at login",
                "quit"
            ]
        );
        assert!(matches!(&r[0], Row::Item { id, .. } if id == "run:p/c1"));
        assert!(matches!(&r[7], Row::Check { checked: true, .. }));
        assert!(working(Some(&status())));
        assert!(!working(Some(&json!({"runs": [], "generating": 0, "queued": 0}))));
        let idle = rows(Some(&json!({"runs": [], "recent": []})), false);
        assert_eq!(idle[0], Row::info("nothing running"));
        assert_eq!(rows(None, false)[0], Row::info("host starting…"));
    }

    #[test]
    fn notices_only_for_new_events() {
        let mut seen = Seen::default();
        assert!(seen.diff(&status()).is_empty(), "the first status is the baseline");
        assert!(seen.diff(&status()).is_empty());
        let mut s = status();
        s["runs"][0]["question"] = json!({"id": "q1", "prompt": "Which model?"});
        let n = seen.diff(&s);
        assert_eq!(n, vec![Notice { project: "p".into(), chat: "c1".into(), title: "Hero shots".into(), body: "Launch · needs you: Which model?".into() }]);
        assert!(seen.diff(&s).is_empty(), "announced once");
        s["runs"] = json!([]);
        s["recent"] = json!([
            {"project": "p", "project_name": "Launch", "chat": "c1", "title": "Hero shots", "state": "error", "ended": 200},
            {"project": "p", "project_name": "Launch", "chat": "c0", "title": "Logo", "state": "idle", "ended": 100},
        ]);
        let n = seen.diff(&s);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].body, "Launch · stopped with an error");
        assert!(seen.diff(&s).is_empty());
    }

    #[test]
    fn quit_summary() {
        assert_eq!(active_summary(&json!({"runs": [], "generating": 0})), None);
        assert_eq!(active_summary(&json!({"runs": [{}, {}]})).unwrap(), "2 agent runs are active");
        assert_eq!(active_summary(&json!({"runs": [{}], "generating": 1})).unwrap(), "1 agent run is active and 1 generation in flight");
        assert_eq!(active_summary(&json!({"runs": [], "generating": 5, "queued": 2})).unwrap(), "7 generations are in flight");
        assert_eq!(active_summary(&json!({"runs": [], "generating": 1})).unwrap(), "1 generation is in flight");
    }
}
