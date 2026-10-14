//! One-click "use fal-os from your own Claude / Codex" (the director setup): registers the
//! fal-os MCP server and installs the `fal-os` skill for each target, shows per-target status
//! (connected / outdated / not installed) and removes exactly what it added.
//!
//! A host that isn't the installed app (a dev build, another `FALOS_HOME`, another port)
//! registers as `fal-os-dev` instead of `fal-os` and leaves the skill alone, so connecting a
//! test host never rewires the user's real agents to it (see [`Ctx::for_host`]).
//!
//! Every writer takes the home directory explicitly (tests use temp dirs). Edits keep the
//! rest of each file: JSON configs keep key order and other keys (and are refused when they
//! don't parse), `config.toml` keeps comments and formatting (`toml_edit`). Writes are atomic
//! (temp file + rename) and keep the file's mode (0600 for new files: they may hold a token).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, Value as Json, json};

use crate::register::SERVER_NAME;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    /// Claude Code (`~/.claude.json`, user scope) + `~/.claude/skills/fal-os/SKILL.md`.
    ClaudeCode,
    /// The Claude desktop app (`claude_desktop_config.json`; stdio servers only).
    ClaudeDesktop,
    /// Codex CLI / app (`~/.codex/config.toml`) + `~/.codex/skills/fal-os/SKILL.md`.
    Codex,
}

impl Target {
    pub const ALL: [Target; 3] = [Target::ClaudeCode, Target::ClaudeDesktop, Target::Codex];

    pub fn id(self) -> &'static str {
        match self {
            Target::ClaudeCode => "claude-code",
            Target::ClaudeDesktop => "claude-desktop",
            Target::Codex => "codex",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Target::ClaudeCode => "Claude Code",
            Target::ClaudeDesktop => "Claude desktop app",
            Target::Codex => "Codex",
        }
    }

    pub fn from_id(s: &str) -> Option<Target> {
        Target::ALL.into_iter().find(|t| t.id() == s)
    }

    pub fn config_path(self, home: &Path) -> PathBuf {
        match self {
            Target::ClaudeCode => home.join(".claude.json"),
            Target::ClaudeDesktop => home.join("Library/Application Support/Claude/claude_desktop_config.json"),
            Target::Codex => home.join(".codex/config.toml"),
        }
    }

    /// Where the `fal-os` skill goes (none for the desktop app: its skills are uploaded in
    /// the app; it gets the same guidance from the server's instructions and `get_knowledge`).
    pub fn skill_path(self, home: &Path) -> Option<PathBuf> {
        match self {
            Target::ClaudeCode => Some(home.join(".claude/skills/fal-os/SKILL.md")),
            Target::ClaudeDesktop => None,
            Target::Codex => Some(home.join(".codex/skills/fal-os/SKILL.md")),
        }
    }

    /// The client looks installed (its home folder exists).
    pub fn detected(self, home: &Path) -> bool {
        match self {
            Target::ClaudeCode => home.join(".claude.json").exists() || home.join(".claude").is_dir(),
            Target::ClaudeDesktop => home.join("Library/Application Support/Claude").is_dir(),
            Target::Codex => home.join(".codex").is_dir(),
        }
    }
}

/// How the client reaches the host.
#[derive(Clone, Debug)]
pub struct Stdio {
    /// Absolute path of `falos-host`.
    pub command: String,
    /// Extra env for the relay (`FALOS_HOME` when the host doesn't use the default).
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct Ctx {
    pub home: PathBuf,
    /// The stdio relay (`falos-host mcp`): survives port and token changes.
    pub stdio: Stdio,
    /// `(url, token)` of the running host: the dev fallback when `prefer_stdio` is false.
    pub http: Option<(String, String)>,
    /// Packaged app (or `FALOS_CONNECT_STDIO=1`): register the stdio relay everywhere.
    pub prefer_stdio: bool,
    /// The MCP server name written: `fal-os`, or `fal-os-dev` for a host that isn't the
    /// installed one.
    pub server: String,
    /// Why this host isn't the installed one ("a dev build · FALOS_HOME=… · port 7417").
    pub instance: Option<String>,
}

/// The server name a non-default host registers under.
pub const DEV_SERVER_NAME: &str = "fal-os-dev";

impl Ctx {
    /// The running host's context: stdio when packaged (the binary sits in an `.app`),
    /// else HTTP with the current URL + token (a dev build's path moves with `cargo clean`).
    ///
    /// Only the installed app (packaged, default `FALOS_HOME`, default port) owns the
    /// `fal-os` entry: any other host writes `fal-os-dev` (`FALOS_CONNECT_NAME` overrides),
    /// so "connect" on a dev or test host can't point the user's Claude Code / Codex at it.
    pub fn for_host(home: PathBuf, http: Option<(String, String)>) -> Ctx {
        let exe = std::env::current_exe().ok();
        let packaged = exe.as_ref().is_some_and(|e| e.to_string_lossy().contains(".app/Contents/MacOS/"));
        let forced = std::env::var("FALOS_CONNECT_STDIO").is_ok_and(|v| v == "1");
        let mut env = vec![];
        let mut why: Vec<String> = vec![];
        if !packaged {
            why.push("a dev build".into());
        }
        if let Some(h) = std::env::var_os("FALOS_HOME") {
            let h = PathBuf::from(h);
            let default = home.join(".fal-os");
            let same = h == default || (h.canonicalize().ok().is_some_and(|c| Some(c) == default.canonicalize().ok()));
            if !same {
                why.push(format!("FALOS_HOME={}", h.display()));
            }
            env.push(("FALOS_HOME".to_string(), h.to_string_lossy().into_owned()));
        }
        if let Some(port) = http.as_ref().and_then(|(u, _)| u.rsplit_once(':')).and_then(|(_, p)| p.split('/').next()?.parse::<u16>().ok())
            && port != crate::server::DEFAULT_PORT
        {
            why.push(format!("port {port}"));
        }
        let instance = (!why.is_empty()).then(|| why.join(" · "));
        let server = std::env::var("FALOS_CONNECT_NAME")
            .ok()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| if instance.is_some() { DEV_SERVER_NAME.into() } else { SERVER_NAME.into() });
        Ctx {
            server,
            instance,
            home,
            stdio: Stdio {
                command: exe.map(|e| e.display().to_string()).unwrap_or_else(|| "falos-host".into()),
                env,
            },
            prefer_stdio: packaged || forced || http.is_none(),
            http,
        }
    }

    /// The skill is the installed app's: other hosts leave it alone.
    pub fn skill_path(&self, t: Target) -> Option<PathBuf> {
        if self.server == SERVER_NAME { t.skill_path(&self.home) } else { None }
    }

    fn use_stdio(&self, t: Target) -> bool {
        // The desktop app's config file only takes stdio servers.
        self.prefer_stdio || t == Target::ClaudeDesktop || self.http.is_none()
    }

    /// Transport for `t`: "stdio" or "http".
    pub fn transport(&self, t: Target) -> &'static str {
        if self.use_stdio(t) { "stdio" } else { "http" }
    }

    /// The MCP server entry for a JSON config (desired keys only).
    fn json_entry(&self, t: Target) -> Map<String, Json> {
        let mut m = Map::new();
        if self.use_stdio(t) {
            if t == Target::ClaudeCode {
                m.insert("type".into(), "stdio".into());
            }
            m.insert("command".into(), self.stdio.command.clone().into());
            m.insert("args".into(), json!(["mcp"]));
            if !self.stdio.env.is_empty() {
                m.insert("env".into(), Json::Object(self.stdio.env.iter().map(|(k, v)| (k.clone(), v.clone().into())).collect()));
            }
        } else {
            let (url, token) = self.http.clone().unwrap_or_default();
            m.insert("type".into(), "http".into());
            m.insert("url".into(), url.into());
            m.insert("headers".into(), json!({"Authorization": format!("Bearer {token}")}));
        }
        m
    }
}

/// Keys that select a transport: replaced (or removed) when we write our entry; anything
/// else the user added (timeouts, …) is kept.
const JSON_TRANSPORT: &[&str] = &["type", "command", "args", "env", "url", "headers", "httpUrl"];
const TOML_TRANSPORT: &[&str] = &["command", "args", "env", "url", "http_headers", "bearer_token_env_var", "env_http_headers", "cwd"];

/// The installed skill: frontmatter + the director doc (knowledge `director`) + a marker that
/// says the file is ours (only marked files are updated or removed).
pub fn skill_text() -> String {
    format!(
        "---\nname: fal-os\ndescription: Direct creative work in fal OS (MCP server `fal-os`): the user's canvas with references, selection and live steering. Generate images/video/audio with fal models in parallel batches, lay results out as boards, take messages from the user mid-run (inbox), report back, drive the in-app agent swarm, export files to other apps and import results with lineage. Use when the user mentions fal OS / fal-os, asks to continue a fal OS project, the canvas, references, boards, moodboards, storyboards, ads, generating media, or when the folder's AGENTS.md / CLAUDE.md says it is a fal OS project.\n---\n\n{}\n{MARKER}\n",
        crate::knowledge::DIRECTOR.trim_end()
    )
}

const MARKER: &str = "<!-- installed by fal-os (connect); fal-os updates or removes this file. Edit freely after deleting this line. -->";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Connected,
    Outdated,
    NotInstalled,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileStatus {
    pub path: String,
    /// "mcp" (the server entry) or "skill".
    pub what: &'static str,
    pub state: State,
    /// What `connect` would do: "create" | "update" | "none"; `disconnect`: "remove" | "none".
    pub connect: &'static str,
    pub disconnect: &'static str,
    /// Why it can't be written (unparseable config, a skill that isn't ours, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TargetStatus {
    pub id: &'static str,
    pub label: &'static str,
    pub detected: bool,
    pub transport: &'static str,
    pub state: State,
    pub files: Vec<FileStatus>,
}

// ---- file plumbing -----------------------------------------------------------------------

fn read(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Atomic write that keeps the existing mode (new files: 0600).
pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let dir = path.parent().context("no parent dir")?;
    std::fs::create_dir_all(dir)?;
    let perms = std::fs::metadata(path).ok().map(|m| m.permissions());
    let tmp = dir.join(format!(".{}.falos-{}", path.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
    std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = perms.unwrap_or_else(|| std::fs::Permissions::from_mode(0o600));
        std::fs::set_permissions(&tmp, p)?;
    }
    #[cfg(not(unix))]
    let _ = perms;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

// ---- JSON configs (Claude Code, Claude desktop) -----------------------------------------

fn parse_json(path: &Path, s: &str) -> Result<Json> {
    if s.trim().is_empty() {
        return Ok(Json::Object(Map::new()));
    }
    let v: Json = serde_json::from_str(s).with_context(|| format!("{} is not valid JSON; fix or edit it by hand (left unchanged)", path.display()))?;
    if !v.is_object() {
        bail!("{}: top level is not an object (left unchanged)", path.display());
    }
    Ok(v)
}

fn json_state(entry: Option<&Json>, want: &Map<String, Json>) -> State {
    let Some(e) = entry.and_then(Json::as_object) else {
        return State::NotInstalled;
    };
    let same = want.iter().all(|(k, v)| e.get(k) == Some(v)) && JSON_TRANSPORT.iter().all(|k| want.contains_key(*k) || !e.contains_key(*k));
    if same { State::Connected } else { State::Outdated }
}

fn to_text(v: &Json, before: &str) -> Result<String> {
    let mut s = serde_json::to_string_pretty(v)?;
    if before.is_empty() || before.ends_with('\n') {
        s.push('\n');
    }
    Ok(s)
}

/// New text with our entry set (`Some`) or removed (`None`); `None` result = no change.
fn json_edit(path: &Path, before: &str, name: &str, want: Option<&Map<String, Json>>) -> Result<Option<String>> {
    let mut root = parse_json(path, before)?;
    let obj = root.as_object_mut().unwrap();
    match want {
        Some(want) => {
            let servers = obj.entry("mcpServers").or_insert_with(|| Json::Object(Map::new()));
            let servers = servers.as_object_mut().with_context(|| format!("{}: `mcpServers` is not an object", path.display()))?;
            if json_state(servers.get(name), want) == State::Connected {
                return Ok(None);
            }
            let e = servers.entry(name).or_insert_with(|| Json::Object(Map::new()));
            if !e.is_object() {
                *e = Json::Object(Map::new());
            }
            let e = e.as_object_mut().unwrap();
            e.retain(|k, _| !JSON_TRANSPORT.contains(&k.as_str()) || want.contains_key(k));
            for (k, v) in want {
                e.insert(k.clone(), v.clone());
            }
        }
        None => {
            let Some(servers) = obj.get_mut("mcpServers").and_then(Json::as_object_mut) else {
                return Ok(None);
            };
            if servers.shift_remove(name).is_none() {
                return Ok(None);
            }
        }
    }
    Ok(Some(to_text(&root, before)?))
}

// ---- Codex config.toml -------------------------------------------------------------------

/// Desired `[mcp_servers.fal-os]` keys.
fn toml_want(ctx: &Ctx) -> Vec<(&'static str, toml_edit::Value)> {
    use toml_edit::{Array, InlineTable, Value};
    let mut v: Vec<(&'static str, Value)> = vec![];
    if ctx.use_stdio(Target::Codex) {
        v.push(("command", ctx.stdio.command.as_str().into()));
        let mut a = Array::new();
        a.push("mcp");
        v.push(("args", Value::Array(a)));
        if !ctx.stdio.env.is_empty() {
            let mut t = InlineTable::new();
            for (k, val) in &ctx.stdio.env {
                t.insert(k, val.as_str().into());
            }
            v.push(("env", Value::InlineTable(t)));
        }
    } else {
        let (url, token) = ctx.http.clone().unwrap_or_default();
        v.push(("url", url.as_str().into()));
        let mut t = InlineTable::new();
        t.insert("Authorization", format!("Bearer {token}").into());
        v.push(("http_headers", Value::InlineTable(t)));
    }
    // Without it Codex asks before every writing tool call and, non-interactively, can't call
    // them at all (same as the in-app Codex driver).
    v.push(("default_tools_approval_mode", "approve".into()));
    // `inbox` long-polls and `await_batch` outlast Codex's 60 s default.
    v.push(("tool_timeout_sec", 600i64.into()));
    v
}

/// Structural equality of TOML values (ignores formatting and comments).
fn toml_eq(a: &toml_edit::Value, b: &toml_edit::Value) -> bool {
    use toml_edit::Value::*;
    match (a, b) {
        (String(x), String(y)) => x.value() == y.value(),
        (Integer(x), Integer(y)) => x.value() == y.value(),
        (Boolean(x), Boolean(y)) => x.value() == y.value(),
        (Array(x), Array(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| toml_eq(p, q)),
        (InlineTable(x), InlineTable(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| toml_eq(v, w))),
        _ => false,
    }
}

fn item_value(i: &toml_edit::Item) -> Option<toml_edit::Value> {
    match i {
        toml_edit::Item::Value(v) => Some(v.clone()),
        toml_edit::Item::Table(t) => Some(toml_edit::Value::InlineTable(t.clone().into_inline_table())),
        _ => None,
    }
}

fn toml_state(doc: &toml_edit::DocumentMut, name: &str, want: &[(&'static str, toml_edit::Value)]) -> State {
    let Some(e) = doc.get("mcp_servers").and_then(|s| s.get(name)).and_then(|e| e.as_table_like()) else {
        return State::NotInstalled;
    };
    let same = want.iter().all(|(k, v)| e.get(k).and_then(item_value).is_some_and(|x| toml_eq(&x, v)))
        && TOML_TRANSPORT.iter().all(|k| want.iter().any(|(w, _)| w == k) || e.get(k).is_none());
    if same { State::Connected } else { State::Outdated }
}

fn parse_toml(path: &Path, s: &str) -> Result<toml_edit::DocumentMut> {
    s.parse().with_context(|| format!("{} is not valid TOML; fix or edit it by hand (left unchanged)", path.display()))
}

fn toml_edit_text(path: &Path, before: &str, name: &str, want: Option<&[(&'static str, toml_edit::Value)]>) -> Result<Option<String>> {
    use toml_edit::{Item, Table};
    let mut doc = parse_toml(path, before)?;
    match want {
        Some(want) => {
            if toml_state(&doc, name, want) == State::Connected {
                return Ok(None);
            }
            let servers = doc
                .entry("mcp_servers")
                .or_insert_with(|| {
                    let mut t = Table::new();
                    t.set_implicit(true);
                    Item::Table(t)
                })
                .as_table_mut()
                .with_context(|| format!("{}: `mcp_servers` is not a table", path.display()))?;
            let entry = servers.entry(name).or_insert_with(|| Item::Table(Table::new()));
            if !entry.is_table() {
                *entry = Item::Table(Table::new());
            }
            let entry = entry.as_table_mut().unwrap();
            let stale: Vec<String> = entry.iter().map(|(k, _)| k.to_string()).filter(|k| TOML_TRANSPORT.contains(&k.as_str()) && !want.iter().any(|(w, _)| w == k)).collect();
            for k in stale {
                entry.remove(&k);
            }
            for (k, v) in want {
                let same = entry.get(k).and_then(item_value).is_some_and(|x| toml_eq(&x, v));
                if !same {
                    entry[*k] = Item::Value(v.clone());
                }
            }
        }
        None => {
            let Some(servers) = doc.get_mut("mcp_servers").and_then(|s| s.as_table_like_mut()) else {
                return Ok(None);
            };
            if servers.remove(name).is_none() {
                return Ok(None);
            }
            if servers.is_empty()
                && let Some(t) = doc.get_mut("mcp_servers").and_then(|s| s.as_table_mut())
            {
                // Only our entry was there: keep the file as it was before us.
                t.set_implicit(true);
            }
        }
    }
    Ok(Some(doc.to_string()))
}

// ---- status / connect / disconnect -------------------------------------------------------

fn mcp_status(t: Target, ctx: &Ctx) -> FileStatus {
    let path = t.config_path(&ctx.home);
    let mut fs = FileStatus { path: path.display().to_string(), what: "mcp", state: State::NotInstalled, connect: "create", disconnect: "none", problem: None };
    let before = match read(&path) {
        Ok(b) => b,
        Err(e) => {
            fs.problem = Some(format!("{e:#}"));
            return fs;
        }
    };
    let exists = before.is_some();
    let before = before.unwrap_or_default();
    let state = match t {
        Target::Codex => parse_toml(&path, &before).map(|d| toml_state(&d, &ctx.server, &toml_want(ctx))),
        _ => parse_json(&path, &before).map(|v| json_state(v.get("mcpServers").and_then(|s| s.get(&ctx.server)), &ctx.json_entry(t))),
    };
    match state {
        Ok(s) => fs.state = s,
        Err(e) => fs.problem = Some(format!("{e:#}")),
    }
    fs.connect = match (fs.state, exists) {
        (State::Connected, _) => "none",
        (_, false) => "create",
        _ => "update",
    };
    fs.disconnect = if fs.state == State::NotInstalled { "none" } else { "remove" };
    fs
}

fn skill_status(path: &Path) -> FileStatus {
    let mut fs = FileStatus { path: path.display().to_string(), what: "skill", state: State::NotInstalled, connect: "create", disconnect: "none", problem: None };
    match read(path) {
        Ok(None) => {}
        Ok(Some(s)) if s == skill_text() => {
            fs.state = State::Connected;
            fs.connect = "none";
            fs.disconnect = "remove";
        }
        Ok(Some(s)) if s.contains(MARKER) => {
            fs.state = State::Outdated;
            fs.connect = "update";
            fs.disconnect = "remove";
        }
        Ok(Some(_)) => {
            // The user's own skill of that name: never touched.
            fs.state = State::Connected;
            fs.connect = "none";
            fs.problem = Some("a fal-os skill that fal-os didn't write is there; it is left alone".into());
        }
        Err(e) => fs.problem = Some(format!("{e:#}")),
    }
    fs
}

pub fn status(t: Target, ctx: &Ctx) -> TargetStatus {
    let mut files = vec![mcp_status(t, ctx)];
    if let Some(p) = ctx.skill_path(t) {
        files.push(skill_status(&p));
    }
    let state = if files.iter().all(|f| f.state == State::Connected) {
        State::Connected
    } else if files.iter().all(|f| f.state == State::NotInstalled) {
        State::NotInstalled
    } else {
        State::Outdated
    };
    TargetStatus { id: t.id(), label: t.label(), detected: t.detected(&ctx.home), transport: ctx.transport(t), state, files }
}

/// Paths changed.
pub fn connect(t: Target, ctx: &Ctx) -> Result<Vec<String>> {
    let mut changed = vec![];
    let path = t.config_path(&ctx.home);
    let before = read(&path)?.unwrap_or_default();
    let after = match t {
        Target::Codex => toml_edit_text(&path, &before, &ctx.server, Some(&toml_want(ctx)))?,
        _ => json_edit(&path, &before, &ctx.server, Some(&ctx.json_entry(t)))?,
    };
    if let Some(a) = after {
        write_atomic(&path, &a)?;
        changed.push(path.display().to_string());
    }
    if let Some(p) = ctx.skill_path(t) {
        let s = skill_status(&p);
        if s.problem.is_none() && s.connect != "none" {
            write_atomic(&p, &skill_text())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // A doc, not a secret.
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644));
            }
            changed.push(p.display().to_string());
        }
    }
    Ok(changed)
}

/// Removes our MCP entry and our skill (only if it carries the marker). Paths changed.
pub fn disconnect(t: Target, ctx: &Ctx) -> Result<Vec<String>> {
    let mut changed = vec![];
    let path = t.config_path(&ctx.home);
    if let Some(before) = read(&path)? {
        let after = match t {
            Target::Codex => toml_edit_text(&path, &before, &ctx.server, None)?,
            _ => json_edit(&path, &before, &ctx.server, None)?,
        };
        if let Some(a) = after {
            write_atomic(&path, &a)?;
            changed.push(path.display().to_string());
        }
    }
    if let Some(p) = ctx.skill_path(t)
        && read(&p)?.is_some_and(|s| s.contains(MARKER))
    {
        std::fs::remove_file(&p)?;
        // The skill folder, if we left it empty.
        if let Some(d) = p.parent() {
            let _ = std::fs::remove_dir(d);
        }
        changed.push(p.display().to_string());
    }
    Ok(changed)
}

/// `{"type":"director.status"|"director.connect"|"director.disconnect", "targets"?: [ids]}`
/// from the canvas UI → `{"type":"director.status", targets:[TargetStatus], changed?, error?}`.
pub fn handle(ctx: &Ctx, msg: &Json) -> Json {
    let kind = msg["type"].as_str().unwrap_or_default();
    let picked: Vec<Target> = msg["targets"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().and_then(Target::from_id)).collect()).unwrap_or_default();
    let mut changed: Vec<String> = vec![];
    let mut errors: Vec<String> = vec![];
    for t in &picked {
        let r = match kind {
            "director.connect" => connect(*t, ctx),
            "director.disconnect" => disconnect(*t, ctx),
            _ => Ok(vec![]),
        };
        match r {
            Ok(c) => changed.extend(c),
            Err(e) => errors.push(format!("{}: {e:#}", t.label())),
        }
    }
    let targets: Vec<TargetStatus> = Target::ALL.iter().map(|t| status(*t, ctx)).collect();
    let mut out = json!({"type": "director.status", "targets": targets, "changed": changed, "home": ctx.home.display().to_string(),
                         "server": ctx.server, "instance": ctx.instance});
    if !errors.is_empty() {
        out["error"] = json!(errors.join("; "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(home: &Path, stdio: bool) -> Ctx {
        Ctx {
            home: home.to_path_buf(),
            stdio: Stdio { command: "/Applications/fal-os.app/Contents/MacOS/falos-host".into(), env: vec![] },
            http: Some(("http://127.0.0.1:7317/mcp".into(), "tok1".into())),
            prefer_stdio: stdio,
            server: SERVER_NAME.into(),
            instance: None,
        }
    }

    #[test]
    fn a_dev_host_registers_beside_the_real_entry() {
        let home = tempfile::tempdir().unwrap();
        let p = home.path().join(".claude.json");
        let real = ctx(home.path(), true);
        connect(Target::ClaudeCode, &real).unwrap();
        connect(Target::Codex, &real).unwrap();
        let claude_before = std::fs::read_to_string(&p).unwrap();
        // A test host (dev build, port 7417): `fal-os-dev`, no skill.
        let mut dev = Ctx::for_host(home.path().to_path_buf(), Some(("http://127.0.0.1:7417/mcp".into(), "tok2".into())));
        assert_eq!(dev.server, DEV_SERVER_NAME);
        assert!(dev.instance.as_deref().is_some_and(|i| i.contains("port 7417")), "{:?}", dev.instance);
        dev.prefer_stdio = false;
        let st = status(Target::ClaudeCode, &dev);
        assert_eq!((st.state, st.files.len()), (State::NotInstalled, 1));
        let ch = connect(Target::ClaudeCode, &dev).unwrap();
        assert_eq!(ch.len(), 1);
        connect(Target::Codex, &dev).unwrap();
        let v: Json = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let ours: Json = serde_json::from_str(&claude_before).unwrap();
        assert_eq!(v["mcpServers"]["fal-os"], ours["mcpServers"]["fal-os"], "the real entry is untouched");
        assert_eq!(v["mcpServers"]["fal-os-dev"]["url"], "http://127.0.0.1:7417/mcp");
        let t = std::fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
        assert!(t.contains("[mcp_servers.fal-os]\ncommand") && t.contains("[mcp_servers.fal-os-dev]\nurl = \"http://127.0.0.1:7417/mcp\""), "{t}");
        assert_eq!(status(Target::ClaudeCode, &real).state, State::Connected, "the real host still sees itself connected");
        // Disconnecting the dev host removes only its entry.
        disconnect(Target::ClaudeCode, &dev).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), claude_before);
        assert!(home.path().join(".claude/skills/fal-os/SKILL.md").is_file());
        let r = handle(&dev, &json!({"type": "director.status"}));
        assert_eq!(r["server"], DEV_SERVER_NAME);
        assert!(r["instance"].is_string());
    }

    #[test]
    fn claude_code_stdio_roundtrip_preserves_everything_else() {
        let home = tempfile::tempdir().unwrap();
        let c = ctx(home.path(), true);
        let p = home.path().join(".claude.json");
        let orig = "{\n  \"numStartups\": 3,\n  \"mcpServers\": {\n    \"other\": {\n      \"command\": \"x\"\n    }\n  },\n  \"projects\": {}\n}\n";
        std::fs::write(&p, orig).unwrap();
        assert_eq!(status(Target::ClaudeCode, &c).state, State::NotInstalled);
        let ch = connect(Target::ClaudeCode, &c).unwrap();
        assert_eq!(ch.len(), 2);
        let v: Json = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v.as_object().unwrap().keys().collect::<Vec<_>>(), ["numStartups", "mcpServers", "projects"]);
        assert_eq!(v["mcpServers"]["fal-os"], json!({"type": "stdio", "command": "/Applications/fal-os.app/Contents/MacOS/falos-host", "args": ["mcp"]}));
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        let s = status(Target::ClaudeCode, &c);
        assert_eq!(s.state, State::Connected, "{s:?}");
        assert!(connect(Target::ClaudeCode, &c).unwrap().is_empty(), "idempotent");
        let skill = std::fs::read_to_string(home.path().join(".claude/skills/fal-os/SKILL.md")).unwrap();
        assert!(skill.starts_with("---\nname: fal-os\ndescription: ") && skill.contains("generate_batch") && skill.contains("import_file"));
        // Disconnect restores the original bytes and removes the skill folder.
        let ch = disconnect(Target::ClaudeCode, &c).unwrap();
        assert_eq!(ch.len(), 2);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), orig);
        assert!(!home.path().join(".claude/skills/fal-os").exists());
        assert_eq!(status(Target::ClaudeCode, &c).state, State::NotInstalled);
    }

    #[test]
    fn http_entry_is_outdated_and_upgraded_in_place() {
        let home = tempfile::tempdir().unwrap();
        let http = ctx(home.path(), false);
        connect(Target::ClaudeCode, &http).unwrap();
        let p = home.path().join(".claude.json");
        let v: Json = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["fal-os"]["type"], "http");
        assert_eq!(status(Target::ClaudeCode, &http).state, State::Connected);
        // Token rotation → outdated.
        let mut rot = ctx(home.path(), false);
        rot.http = Some(("http://127.0.0.1:7318/mcp".into(), "tok2".into()));
        assert_eq!(status(Target::ClaudeCode, &rot).state, State::Outdated);
        // Switch to stdio: url/headers go, user extras stay.
        let mut v = v;
        v["mcpServers"]["fal-os"]["timeout"] = json!(5);
        std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        let st = ctx(home.path(), true);
        assert_eq!(status(Target::ClaudeCode, &st).files[0].state, State::Outdated);
        connect(Target::ClaudeCode, &st).unwrap();
        let v: Json = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let e = &v["mcpServers"]["fal-os"];
        assert!(e.get("url").is_none() && e.get("headers").is_none());
        assert_eq!(e["timeout"], 5);
        assert_eq!(e["command"], "/Applications/fal-os.app/Contents/MacOS/falos-host");
        // Outdated skill (ours, older text) is updated; a foreign one is left alone.
        let sp = home.path().join(".claude/skills/fal-os/SKILL.md");
        std::fs::write(&sp, format!("old\n{MARKER}\n")).unwrap();
        assert_eq!(status(Target::ClaudeCode, &st).state, State::Outdated);
        connect(Target::ClaudeCode, &st).unwrap();
        assert_eq!(std::fs::read_to_string(&sp).unwrap(), skill_text());
        std::fs::write(&sp, "mine").unwrap();
        connect(Target::ClaudeCode, &st).unwrap();
        disconnect(Target::ClaudeCode, &st).unwrap();
        assert_eq!(std::fs::read_to_string(&sp).unwrap(), "mine");
    }

    #[test]
    fn desktop_is_always_stdio_and_refuses_bad_json() {
        let home = tempfile::tempdir().unwrap();
        let c = ctx(home.path(), false);
        let p = Target::ClaudeDesktop.config_path(home.path());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{\"preferences\": {\"x\": 1}}").unwrap();
        connect(Target::ClaudeDesktop, &c).unwrap();
        let v: Json = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["preferences"]["x"], 1);
        assert_eq!(v["mcpServers"]["fal-os"], json!({"command": "/Applications/fal-os.app/Contents/MacOS/falos-host", "args": ["mcp"]}));
        assert_eq!(status(Target::ClaudeDesktop, &c).state, State::Connected);
        assert_eq!(status(Target::ClaudeDesktop, &c).files.len(), 1);
        std::fs::write(&p, "{ broken").unwrap();
        assert!(connect(Target::ClaudeDesktop, &c).is_err());
        assert!(disconnect(Target::ClaudeDesktop, &c).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ broken");
        assert!(status(Target::ClaudeDesktop, &c).files[0].problem.is_some());
    }

    #[test]
    fn codex_toml_keeps_comments_and_approval_mode() {
        let home = tempfile::tempdir().unwrap();
        let p = home.path().join(".codex/config.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let orig = "# mine\nmodel = \"gpt-5\"  # fav\n\n[mcp_servers.docs]\ncommand = \"docs\"\n";
        std::fs::write(&p, orig).unwrap();
        let st = ctx(home.path(), true);
        connect(Target::Codex, &st).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.starts_with(orig), "{t}");
        assert!(t.contains("[mcp_servers.fal-os]\ncommand = \"/Applications/fal-os.app/Contents/MacOS/falos-host\"\nargs = [\"mcp\"]\ndefault_tools_approval_mode = \"approve\"\n"), "{t}");
        assert_eq!(status(Target::Codex, &st).state, State::Connected);
        assert!(home.path().join(".codex/skills/fal-os/SKILL.md").is_file());
        assert!(connect(Target::Codex, &st).unwrap().is_empty());
        disconnect(Target::Codex, &st).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), orig);

        // An old http registration (from `register`) is outdated, then replaced by stdio
        // keeping the user's extra keys.
        std::fs::write(&p, format!("{orig}\n[mcp_servers.fal-os]\nurl = \"http://127.0.0.1:7317/mcp\"\nhttp_headers = {{ Authorization = \"Bearer t\" }}\nstartup_timeout_sec = 20\n")).unwrap();
        assert_eq!(status(Target::Codex, &st).files[0].state, State::Outdated);
        connect(Target::Codex, &st).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(!t.contains("http_headers") && !t.contains("url =") && t.contains("startup_timeout_sec = 20") && t.contains("args = [\"mcp\"]"), "{t}");
        // http fallback (dev).
        let http = ctx(home.path(), false);
        connect(Target::Codex, &http).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        let t = t.split("[mcp_servers.fal-os]").nth(1).unwrap();
        assert!(t.contains("url = \"http://127.0.0.1:7317/mcp\"") && !t.contains("command =") && t.contains("default_tools_approval_mode = \"approve\""), "{t}");
        std::fs::write(&p, "not = = toml").unwrap();
        assert!(connect(Target::Codex, &st).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "not = = toml");
    }

    #[test]
    fn handle_reports_status_for_all_targets() {
        let home = tempfile::tempdir().unwrap();
        let c = ctx(home.path(), true);
        let r = handle(&c, &json!({"type": "director.connect", "targets": ["claude-code", "codex"]}));
        assert_eq!(r["targets"].as_array().unwrap().len(), 3);
        assert_eq!(r["changed"].as_array().unwrap().len(), 4);
        assert_eq!(r["targets"][0]["state"], "connected");
        assert_eq!(r["targets"][1]["state"], "not-installed");
        assert_eq!(r["targets"][2]["state"], "connected");
        assert!(!r.to_string().contains("tok1"));
    }
}
