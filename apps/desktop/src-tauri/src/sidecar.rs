//! The packaged app's local services: the `falos-host` sidecar (which supervises the agent
//! runtime) and the first-run setup helpers.
//!
//! Bundle layout (built by `scripts/package-macos.sh`; the same place Tauri's `externalBin`
//! puts sidecars):
//! - `Contents/MacOS/falos-host`: the host binary.
//! - `Contents/Resources/runtime/`: the agent runtime, bundled (`dist/main.mjs`, `dist/setup.mjs`),
//!   plus the template sources and `scripts/` it reads.
//! - `Contents/Resources/node/`: a Node 22 runtime (`bin/node`, `lib/node_modules/npm`).
//!
//! Without that layout (`cargo run` from a checkout) nothing is spawned and the app finds a host
//! you started yourself through `$FALOS_HOME/host.json`, as before.
//!
//! The host runs in its own process group with `FALOS_EXIT_WITH_PARENT=1`; quitting the app
//! SIGTERMs the group (the host then stops the runtime's group), and a crashed app leaves
//! nothing behind (the host notices it was reparented and exits; the runtime follows its host).
//!
//! No macOS permission prompts on the way (docs/PACKAGING.md §2.5.1): the fal key lives in
//! `$FALOS_HOME/fal.env` (0600), not the Keychain, whose "wants to use your confidential
//! information" dialog an ad-hoc build would trigger again after every update; every child runs
//! with its cwd in `$FALOS_HOME`, never in a protected folder.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};

const KEYCHAIN_SERVICE: &str = "ai.fal.os";
const KEYCHAIN_ACCOUNT: &str = "FAL_KEY";

pub struct Sidecar {
    /// `Contents/Resources` when running from a packaged bundle with the runtime inside.
    resources: Option<PathBuf>,
    host_bin: Option<PathBuf>,
    pub home: PathBuf,
    path_env: OnceLock<String>,
    host: Mutex<Option<Child>>,
    setup: Mutex<Option<Child>>,
    tasks: Mutex<HashMap<String, Arc<Mutex<Task>>>>,
}

pub fn falos_home() -> PathBuf {
    std::env::var_os("FALOS_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".fal-os"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

/// The user's login-shell PATH. Apps started from Finder get only `/usr/bin:/bin:/usr/sbin:/sbin`,
/// which misses Homebrew (`ffmpeg`), `~/.local/bin` (`claude`), nvm/volta (`codex`), …
///
/// A login shell but not an interactive one (`.zprofile`, not `.zshrc`), started in the home folder:
/// whatever an interactive rc file touches (dotfiles synced via iCloud Drive or Documents, prompt
/// plugins scanning the cwd) would raise a macOS folder-access prompt in fal-os's name. Node
/// managers that only `.zshrc` sets up are covered by the fixed dirs below.
fn login_path() -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut found = String::new();
    if let Ok(mut c) = Command::new(&shell)
        .args(["-lc", "printf '\\n__FALOS_PATH__%s__FALOS_PATH__\\n' \"$PATH\""])
        .current_dir(home())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        let start = Instant::now();
        loop {
            match c.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < Duration::from_secs(4) => std::thread::sleep(Duration::from_millis(30)),
                _ => {
                    let _ = c.kill();
                    break;
                }
            }
        }
        let mut out = String::new();
        if let Some(mut so) = c.stdout.take() {
            let _ = so.read_to_string(&mut out);
        }
        if let Some(p) = out.split("__FALOS_PATH__").nth(1) {
            found = p.to_string();
        }
    }
    let h = home();
    let mut dirs: Vec<String> = found.split(':').filter(|d| !d.is_empty()).map(String::from).collect();
    // nvm's newest Node (where `npm i -g` puts claude / codex for nvm users).
    let nvm = std::fs::read_dir(h.join(".nvm/versions/node"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("bin"))
        .filter(|b| b.is_dir())
        .max_by_key(|b| node_version(b));
    for d in [
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".into(),
        h.join(".local/bin").display().to_string(),
        h.join(".claude/local").display().to_string(),
        h.join(".volta/bin").display().to_string(),
        h.join(".bun/bin").display().to_string(),
        h.join(".npm-global/bin").display().to_string(),
    ]
    .into_iter()
    .chain(nvm.map(|b| b.display().to_string()))
    .chain([
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
    ]) {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs.join(":")
}

/// `…/v22.11.0/bin` → (22, 11, 0) for picking nvm's newest.
fn node_version(bin: &Path) -> (u32, u32, u32) {
    let v = bin.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut it = v.trim_start_matches('v').split('.').map(|x| x.parse().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

impl Sidecar {
    pub fn new() -> Self {
        let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
        let resources = exe_dir
            .as_ref()
            .map(|d| d.join("../Resources"))
            .filter(|r| r.join("runtime/dist/main.mjs").exists() && r.join("node/bin/node").exists())
            .filter(|_| std::env::var_os("FALOS_NO_SIDECAR").is_none());
        let host_bin = exe_dir.map(|d| d.join("falos-host")).filter(|b| b.exists());
        Self::with_layout(resources, host_bin, falos_home())
    }

    fn with_layout(resources: Option<PathBuf>, host_bin: Option<PathBuf>, home: PathBuf) -> Self {
        let (path_env, host, setup, tasks) = Default::default();
        Self { resources, host_bin, home, path_env, host, setup, tasks }
    }

    /// Bundled Node first (agents run `npx remotion …` / `node …` with a known-good Node 22),
    /// then what the setup screen installs (`$FALOS_HOME/bin`: ffmpeg + ffprobe,
    /// `$FALOS_HOME/npm/bin`: codex), then the login shell's PATH. Computed once, off the main
    /// thread (`start` runs on its own).
    fn path_env(&self, resources: &Path) -> &str {
        self.path_env.get_or_init(|| {
            format!(
                "{}:{}:{}:{}",
                resources.join("node/bin").display(),
                self.home.join("bin").display(),
                self.home.join("npm/bin").display(),
                login_path()
            )
        })
    }

    pub fn packaged(&self) -> bool {
        self.resources.is_some() && self.host_bin.is_some()
    }

    /// Env shared by the host (→ runtime) and the setup helper.
    fn env(&self, cmd: &mut Command) {
        let Some(r) = &self.resources else { return };
        let templates = self.home.join("templates");
        cmd.env("PATH", self.path_env(r))
            .env("FALOS_HOME", &self.home)
            .env("FALOS_PACKAGED", "1")
            .env("FALOS_NODE", r.join("node/bin/node"))
            .env("FALOS_MOTION_TEMPLATE", templates.join("motion-template"))
            .env("FALOS_HYPERFRAMES_TEMPLATE", templates.join("hyperframes-template"))
            .env("FALOS_SKILLS_DIR", self.home.join("skills"))
            // The runtime must never inherit a key from the app's own environment by accident.
            .env_remove("FAL_KEY");
        // Children run in our home: a cwd inherited from wherever the app was started could be a
        // protected folder (Desktop, Documents, a removable volume).
        let _ = std::fs::create_dir_all(&self.home);
        cmd.current_dir(&self.home);
        if let Some(k) = self.stored_key() {
            cmd.env("FAL_KEY", k);
        }
    }

    /// The fal key from `$FALOS_HOME/fal.env` (the same file the host and the runtime read).
    fn stored_key(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.home.join("fal.env")).ok()?;
        text.lines().find_map(|l| {
            let v = l.trim().trim_start_matches("export ").strip_prefix("FAL_KEY")?.trim_start().strip_prefix('=')?;
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
            (!v.is_empty()).then(|| v.to_string())
        })
    }

    fn log_file(&self, name: &str) -> Stdio {
        let dir = self.home.join("logs");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(name))
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null())
    }

    /// Start the host (no-op when not packaged or already running).
    pub fn start(&self) -> Result<(), String> {
        let (Some(r), Some(bin)) = (&self.resources, &self.host_bin) else { return Ok(()) };
        let mut slot = self.host.lock().unwrap();
        if let Some(c) = slot.as_mut()
            && matches!(c.try_wait(), Ok(None))
        {
            return Ok(());
        }
        std::fs::create_dir_all(&self.home).map_err(|e| e.to_string())?;
        strip_quarantine(&r.join("../.."));
        let mut cmd = Command::new(bin);
        cmd.arg("serve").arg("--runtime").arg(r.join("runtime"));
        // A different projects folder than ~/fal-os-projects (tests, or several installs).
        if let Some(w) = std::env::var_os("FALOS_PROJECTS") {
            cmd.arg("--workspace").arg(w);
        }
        // A preferred port other than 7317 (a test build next to the installed app, whose agents'
        // MCP configs point at 7317).
        if let Some(p) = std::env::var("FALOS_PORT").ok().filter(|p| p.parse::<u16>().is_ok()) {
            cmd.arg("--port").arg(p);
        }
        self.env(&mut cmd);
        cmd.env("FALOS_EXIT_WITH_PARENT", "1")
            .stdin(Stdio::null())
            .stdout(self.log_file("host.log"))
            .stderr(self.log_file("host.log"));
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let child = cmd.spawn().map_err(|e| format!("could not start falos-host: {e}"))?;
        *slot = Some(child);
        Ok(())
    }

    /// SIGTERM the host's process group, wait up to 5 s, then SIGKILL.
    pub fn stop(&self) {
        let tasks: Vec<String> = self.tasks.lock().unwrap().keys().cloned().collect();
        for name in tasks {
            self.cancel_task(&name);
        }
        for slot in [&self.setup, &self.host] {
            let Some(mut c) = slot.lock().unwrap().take() else { continue };
            let group = format!("-{}", c.id());
            let _ = Command::new("kill").args(["-TERM", &group]).status();
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(5) {
                if !matches!(c.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if matches!(c.try_wait(), Ok(None)) {
                let _ = Command::new("kill").args(["-KILL", &group]).status();
                let _ = c.wait();
            }
        }
    }

    pub fn restart(&self) -> Result<(), String> {
        if let Some(mut c) = self.host.lock().unwrap().take() {
            let _ = Command::new("kill").args(["-TERM", &format!("-{}", c.id())]).status();
            let start = Instant::now();
            while matches!(c.try_wait(), Ok(None)) && start.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = c.kill();
            let _ = c.wait();
        }
        self.start()
    }

    fn host_pid(&self) -> Option<u32> {
        let mut slot = self.host.lock().unwrap();
        let c = slot.as_mut()?;
        matches!(c.try_wait(), Ok(None)).then(|| c.id())
    }

    /// `{port, token, project}` from `$FALOS_HOME/host.json`. When we run the host, wait (up to
    /// 20 s) until the file is the one our host wrote, so the UI never connects to a stale port.
    pub fn host_info(&self) -> Option<Json> {
        let start = Instant::now();
        loop {
            let v: Option<Json> = std::fs::read(self.home.join("host.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            let ours = match (self.packaged(), self.host_pid(), &v) {
                (false, _, _) => true,
                (true, Some(pid), Some(v)) => v["pid"].as_u64() == Some(pid as u64),
                _ => false,
            };
            if ours && let Some(v) = v {
                return Some(json!({ "port": v.get("port")?, "token": v.get("token")?, "project": v.get("project") }));
            }
            if !self.packaged() || start.elapsed() > Duration::from_secs(20) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn node(&self, args: &[&str], fal_key: Option<&str>) -> Result<Json, String> {
        let r = self.resources.as_ref().ok_or("not running from the packaged app")?;
        let mut cmd = Command::new(r.join("node/bin/node"));
        cmd.arg(r.join("runtime/dist/setup.mjs")).args(args);
        self.env(&mut cmd);
        if let Some(k) = fal_key {
            cmd.env("FAL_KEY", k);
        }
        let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout);
        let last = text.lines().rev().find(|l| l.starts_with('{')).ok_or("setup helper gave no answer")?;
        serde_json::from_str(last).map_err(|e| e.to_string())
    }

    /// Everything the setup screen shows.
    pub fn status(&self) -> Json {
        if !self.packaged() {
            return json!({ "packaged": false });
        }
        let mut v = self.node(&["doctor"], None).unwrap_or_else(|e| json!({ "error": e }));
        v["packaged"] = json!(true);
        let file = self.stored_key().is_some();
        v["keySource"] = json!(if file { "file" } else { "none" });
        // A key an older build kept in the Keychain: offered as a one-click move (it costs one
        // macOS prompt, explained on the setup screen). Found by its attributes only: no prompt.
        v["keychainKey"] = json!(!file && keychain_has_key());
        v["home"] = json!(self.home.display().to_string());
        v["installing"] = json!(self.setup_running());
        v
    }

    pub fn check_key(&self, key: Option<&str>) -> Result<Json, String> {
        self.node(&["check-key"], key)
    }

    fn setup_running(&self) -> bool {
        self.setup.lock().unwrap().as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    /// Install the motion tools (templates + npm deps, chrome-headless-shell, skills) in the
    /// background; progress lands in `$FALOS_HOME/setup-state.json` (see `status`).
    pub fn install(&self) -> Result<(), String> {
        let r = self.resources.as_ref().ok_or("not running from the packaged app")?;
        if self.setup_running() {
            return Ok(());
        }
        let mut cmd = Command::new(r.join("node/bin/node"));
        cmd.arg(r.join("runtime/dist/setup.mjs")).arg("install");
        self.env(&mut cmd);
        cmd.stdin(Stdio::null())
            .stdout(self.log_file("setup.log"))
            .stderr(self.log_file("setup.log"));
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        *self.setup.lock().unwrap() = Some(cmd.spawn().map_err(|e| e.to_string())?);
        Ok(())
    }

    /// A login tool: `$FALOS_<NAME>_PATH`, then our own installs, then the PATH the host gets.
    fn find_tool(&self, name: &str) -> Option<PathBuf> {
        if let Some(p) = std::env::var_os(format!("FALOS_{}_PATH", name.to_uppercase())) {
            return Some(p.into());
        }
        let r = self.resources.as_ref()?;
        self.path_env(r).split(':').map(|d| Path::new(d).join(name)).find(|p| p.is_file())
    }

    /// Start one of the setup screen's fixed tasks (never an arbitrary command) in the
    /// background; `task` reports its output. A finished install restarts the host, so the
    /// runtime sees the new `claude` / `ffmpeg` / `codex`.
    pub fn run_task(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let r = self.resources.clone().ok_or("not running from the packaged app")?;
        // Held until the task is registered: a double click starts it once.
        let mut tasks = self.tasks.lock().unwrap();
        if tasks.get(name).is_some_and(|t| t.lock().unwrap().running) {
            return Ok(());
        }
        let setup = |sub: &str| {
            let mut c = Command::new(r.join("node/bin/node"));
            c.arg(r.join("runtime/dist/setup.mjs")).arg(sub);
            c
        };
        let tool = |n: &str| self.find_tool(n).ok_or_else(|| format!("{n} isn't installed yet"));
        let mut cmd = match name {
            // Anthropic's official installer: everything goes under $HOME (~/.local/bin/claude).
            "claude-install" => {
                let mut c = Command::new("/bin/bash");
                c.args(["-c", "set -o pipefail; curl -fsSL https://claude.ai/install.sh | bash"]);
                c
            }
            // Opens the browser; the CLI waits for the OAuth callback on localhost.
            "claude-login" => {
                let mut c = Command::new(tool("claude")?);
                c.args(["auth", "login"]);
                c
            }
            "ffmpeg" => setup("ffmpeg"),
            "codex-install" => setup("codex"),
            "codex-login" => {
                let mut c = Command::new(tool("codex")?);
                c.arg("login");
                c
            }
            _ => return Err(format!("unknown setup task {name}")),
        };
        self.env(&mut cmd);
        // Installers and logins have no business with the fal key.
        cmd.env_remove("FAL_KEY");
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        std::fs::create_dir_all(self.home.join("logs")).map_err(|e| e.to_string())?;
        let mut child = cmd.spawn().map_err(|e| format!("{name}: {e}"))?;
        let state = Arc::new(Mutex::new(Task { running: true, pid: child.id(), ..Task::default() }));
        let log = Arc::new(Mutex::new(
            std::fs::OpenOptions::new().create(true).append(true).open(self.home.join("logs/setup.log")).ok(),
        ));
        let readers: Vec<_> = [child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>), child.stderr.take().map(|e| Box::new(e) as _)]
            .into_iter()
            .flatten()
            .map(|out| {
                let (state, log, name) = (state.clone(), log.clone(), name.to_string());
                std::thread::spawn(move || pump(out, &state, &log, &name))
            })
            .collect();
        tasks.insert(name.to_string(), state.clone());
        drop(tasks);
        let (side, name) = (self.clone(), name.to_string());
        std::thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code());
            {
                let mut t = state.lock().unwrap();
                t.running = false;
                t.code = Some(code.unwrap_or(-1));
            }
            // The rest of the output (a helper the tool left running may hold the pipes; the
            // task counts as done either way).
            for r in readers {
                let _ = r.join();
            }
            if code == Some(0) && matches!(name.as_str(), "claude-install" | "ffmpeg" | "codex-install") {
                let _ = side.restart();
            }
        });
        Ok(())
    }

    /// `{running, code, lines, url}` of a setup task (null if it never ran). `url` is the first
    /// https link in its output (the login page, when the browser didn't open by itself).
    pub fn task(&self, name: &str) -> Json {
        let Some(t) = self.tasks.lock().unwrap().get(name).cloned() else { return Json::Null };
        let t = t.lock().unwrap();
        json!({ "running": t.running, "code": t.code, "lines": t.lines, "url": t.url })
    }

    pub fn cancel_task(&self, name: &str) {
        let Some(t) = self.tasks.lock().unwrap().get(name).cloned() else { return };
        let t = t.lock().unwrap();
        if t.running {
            let _ = Command::new("kill").args(["-TERM", &format!("-{}", t.pid)]).status();
        }
    }

    /// Store the fal key in `$FALOS_HOME/fal.env` (0600), not the Keychain: an ad-hoc signed build
    /// has a new code signature after every update, and the Keychain would ask "fal-os wants to
    /// use your confidential information" each time. Restarts the host so the runtime picks it up.
    pub fn save_key(&self, key: &str) -> Result<&'static str, String> {
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) || key.len() > 512 {
            return Err("that doesn't look like a fal key".into());
        }
        write_private(&self.home.join("fal.env"), &format!("FAL_KEY={key}\n"))?;
        self.restart()?;
        Ok("file")
    }

    /// Move a key an older build stored in the Keychain into `fal.env`. Reading it is the one
    /// Keychain prompt left, and only on this click. The Keychain copy stays (deleting it could
    /// prompt again); [remove] deletes it.
    pub fn migrate_key(&self) -> Result<&'static str, String> {
        let key = keychain_read().ok_or("no fal key in the Keychain (or access was denied)")?;
        self.save_key(&key)
    }

    pub fn forget_key(&self) -> Result<(), String> {
        let _ = std::fs::remove_file(self.home.join("fal.env"));
        if keychain_has_key() {
            keychain_delete();
        }
        self.restart()
    }
}

/// A setup task's live state (see `Sidecar::run_task`).
#[derive(Default)]
struct Task {
    running: bool,
    pid: u32,
    code: Option<i32>,
    /// The last lines of output (stdout + stderr), ANSI stripped; `\r` progress counts as a line.
    lines: Vec<String>,
    url: Option<String>,
}

fn pump(mut out: Box<dyn Read + Send>, state: &Mutex<Task>, log: &Mutex<Option<std::fs::File>>, name: &str) {
    use std::io::Write;
    let mut buf = [0u8; 4096];
    let mut line = Vec::new();
    let push = |raw: &[u8]| {
        let text = strip_ansi(&String::from_utf8_lossy(raw));
        let text = text.trim_end();
        if text.trim().is_empty() {
            return;
        }
        if let Some(f) = log.lock().unwrap().as_mut() {
            let _ = writeln!(f, "[{name}] {text}");
        }
        let mut t = state.lock().unwrap();
        if t.url.is_none()
            && let Some(i) = text.find("https://")
        {
            let u: String = text[i..].chars().take_while(|c| !c.is_whitespace() && !"\"'<>)".contains(*c)).collect();
            t.url = Some(u);
        }
        t.lines.push(text.chars().take(300).collect());
        let n = t.lines.len();
        if n > 40 {
            t.lines.drain(..n - 40);
        }
    };
    while let Ok(n) = out.read(&mut buf) {
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                push(&line);
                line.clear();
            } else {
                line.push(b);
            }
        }
    }
    push(&line);
}

/// Drop terminal escape sequences (colors, cursor moves) from CLI output.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match it.peek() {
            Some('[') => {
                it.next();
                for c in it.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC (e.g. hyperlinks): up to BEL or ESC \
                it.next();
                while let Some(c) = it.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && it.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {
                it.next();
            }
        }
    }
    out
}

/// The Keychain item older builds stored the key in (`ai.fal.os` / `FAL_KEY`), unless disabled
/// (`FALOS_NO_KEYCHAIN`, tests next to an installed app).
fn keychain_service() -> Option<String> {
    if std::env::var_os("FALOS_NO_KEYCHAIN").is_some() {
        return None;
    }
    Some(std::env::var("FALOS_KEYCHAIN_SERVICE").unwrap_or_else(|_| KEYCHAIN_SERVICE.into()))
}

/// Whether that item exists. Asks for its attributes only, never its secret, so macOS shows no
/// access prompt.
#[cfg(target_os = "macos")]
fn keychain_has_key() -> bool {
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
    let Some(service) = keychain_service() else { return false };
    ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(&service)
        .account(KEYCHAIN_ACCOUNT)
        .load_attributes(true)
        .limit(Limit::Max(1))
        .search()
        .is_ok_and(|r| !r.is_empty())
}

/// The secret itself: macOS asks the user (once per signature) unless this build created it.
#[cfg(target_os = "macos")]
fn keychain_read() -> Option<String> {
    let b = security_framework::passwords::get_generic_password(&keychain_service()?, KEYCHAIN_ACCOUNT).ok()?;
    String::from_utf8(b).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

#[cfg(target_os = "macos")]
fn keychain_delete() {
    if let Some(service) = keychain_service() {
        let _ = security_framework::passwords::delete_generic_password(&service, KEYCHAIN_ACCOUNT);
    }
}

#[cfg(not(target_os = "macos"))]
fn keychain_has_key() -> bool {
    false
}
#[cfg(not(target_os = "macos"))]
fn keychain_read() -> Option<String> {
    None
}
#[cfg(not(target_os = "macos"))]
fn keychain_delete() {}

/// Drop `com.apple.quarantine` from our own bundle once the user has opened it (the xattr step,
/// or "Open Anyway"): otherwise Gatekeeper may assess the nested `falos-host` and `node` on their
/// own and block them with a second "can't be opened" dialog. A no-op when the bundle isn't ours
/// to change (a translocated, read-only copy) or carries no quarantine.
fn strip_quarantine(bundle: &Path) {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        const NAME: &[u8] = b"com.apple.quarantine\0";
        fn has(p: &Path) -> bool {
            let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
            // SAFETY: valid C strings; a null buffer asks for the size only.
            unsafe { libc::getxattr(c.as_ptr(), NAME.as_ptr().cast(), std::ptr::null_mut(), 0, 0, libc::XATTR_NOFOLLOW) >= 0 }
        }
        fn walk(p: &Path) {
            if let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) {
                // SAFETY: valid C strings.
                unsafe { libc::removexattr(c.as_ptr(), NAME.as_ptr().cast(), libc::XATTR_NOFOLLOW) };
            }
            if p.symlink_metadata().is_ok_and(|m| m.is_dir()) {
                for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
                    walk(&e.path());
                }
            }
        }
        let Ok(bundle) = bundle.canonicalize() else { return };
        if bundle.extension().is_some_and(|e| e == "app") && has(&bundle) {
            walk(&bundle);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = bundle;
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi() {
        assert_eq!(strip_ansi("\u{1b}[1;32mok\u{1b}[0m done"), "ok done");
        assert_eq!(strip_ansi("\u{1b}]8;;https://x\u{7}link\u{1b}]8;;\u{1b}\\ end"), "link end");
    }

    /// The key goes to `fal.env` (0600), never the Keychain, and children get it in their env.
    #[test]
    fn key_lives_in_a_private_file() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: tests in this module don't read the Keychain env vars concurrently.
        unsafe { std::env::set_var("FALOS_NO_KEYCHAIN", "1") };
        let root = std::env::temp_dir().join(format!("falos-key-test-{}", std::process::id()));
        let side = Sidecar::with_layout(None, None, root.clone());
        assert!(side.save_key("has space").is_err());
        assert_eq!(side.save_key(" abc:def \n").unwrap(), "file");
        let f = root.join("fal.env");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "FAL_KEY=abc:def\n");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(side.stored_key().as_deref(), Some("abc:def"));
        assert!(!keychain_has_key());
        assert!(side.migrate_key().is_err());
        side.forget_key().unwrap();
        assert!(side.stored_key().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Quarantine comes off the whole bundle once it's opened.
    #[test]
    fn strips_own_quarantine() {
        use std::os::unix::ffi::OsStrExt;
        let root = std::env::temp_dir().join(format!("falos-q-test-{}", std::process::id()));
        let app = root.join("x.app");
        let bin = app.join("Contents/MacOS/bin");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        std::fs::write(&bin, b"x").unwrap();
        let q = |p: &Path, set: bool| {
            let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
            let n = c"com.apple.quarantine";
            let v = b"0081;00000000;Test;";
            unsafe {
                if set {
                    libc::setxattr(c.as_ptr(), n.as_ptr(), v.as_ptr().cast(), v.len(), 0, 0) == 0
                } else {
                    libc::getxattr(c.as_ptr(), n.as_ptr(), std::ptr::null_mut(), 0, 0, 0) >= 0
                }
            }
        };
        assert!(q(&app, true) && q(&bin, true));
        strip_quarantine(&app.join("Contents/Resources/../.."));
        assert!(!q(&app, false) && !q(&bin, false));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A login task runs the tool found on the app's PATH with fixed args (no fal key in its
    /// env), streams its output ANSI-stripped and picks up the sign-in URL. A stub stands in
    /// for `claude`.
    #[test]
    fn login_task_streams_output() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("falos-sidecar-test-{}", std::process::id()));
        let bin = root.join("res/node/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let stub = bin.join("claude");
        let script = r#"#!/bin/sh
echo 'Opening browser to sign in'
printf '\033[2mvisit https://claude.ai/oauth/authorize?code=true&x=1\033[0m\r\n'
echo "args: $*"
[ -z "$FAL_KEY" ] && echo 'no fal key'
exit 0
"#;
        std::fs::write(&stub, script).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let side = Arc::new(Sidecar::with_layout(Some(root.join("res")), None, root.join("home")));
        assert!(side.run_task("rm -rf").is_err());
        side.run_task("claude-login").unwrap();
        let start = Instant::now();
        while side.task("claude-login")["running"] == json!(true) && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(300)); // the readers drain after the exit
        let t = side.task("claude-login");
        assert_eq!(t["code"], json!(0), "{t}");
        let lines: Vec<&str> = t["lines"].as_array().unwrap().iter().filter_map(|l| l.as_str()).collect();
        assert!(lines.contains(&"args: auth login"), "{lines:?}");
        assert!(lines.contains(&"no fal key"), "{lines:?}");
        assert_eq!(t["url"], json!("https://claude.ai/oauth/authorize?code=true&x=1"));
        let log = std::fs::read_to_string(root.join("home/logs/setup.log")).unwrap();
        assert!(log.contains("[claude-login] args: auth login"), "{log}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
