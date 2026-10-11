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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
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
}

pub fn falos_home() -> PathBuf {
    std::env::var_os("FALOS_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".fal-os"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

/// The user's login-shell PATH. Apps started from Finder get only `/usr/bin:/bin:/usr/sbin:/sbin`,
/// which misses Homebrew (`ffmpeg`), `~/.local/bin` (`claude`), nvm/volta (`codex`), …
fn login_path() -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut found = String::new();
    if let Ok(mut c) = Command::new(&shell)
        .args(["-ilc", "printf '\\n__FALOS_PATH__%s__FALOS_PATH__\\n' \"$PATH\""])
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
    for d in [
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".into(),
        h.join(".local/bin").display().to_string(),
        h.join(".claude/local").display().to_string(),
        h.join(".volta/bin").display().to_string(),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
    ] {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs.join(":")
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
        Self { resources, host_bin, home: falos_home(), path_env: OnceLock::new(), host: Mutex::default(), setup: Mutex::default() }
    }

    /// Bundled Node first (agents run `npx remotion …` / `node …` with a known-good Node 22),
    /// then the login shell's PATH. Computed once, off the main thread (`start` runs on its own).
    fn path_env(&self, resources: &Path) -> &str {
        self.path_env.get_or_init(|| format!("{}:{}", resources.join("node/bin").display(), login_path()))
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
        if let Some(k) = keychain_get() {
            cmd.env("FAL_KEY", k);
        }
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
        let mut cmd = Command::new(bin);
        cmd.arg("serve").arg("--runtime").arg(r.join("runtime"));
        // A different projects folder than ~/fal-os-projects (tests, or several installs).
        if let Some(w) = std::env::var_os("FALOS_PROJECTS") {
            cmd.arg("--workspace").arg(w);
        }
        self.env(&mut cmd);
        cmd.env("FALOS_EXIT_WITH_PARENT", "1")
            .current_dir(&self.home)
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
        v["keySource"] = json!(if keychain_get().is_some() {
            "keychain"
        } else if v["falKey"] == json!(true) {
            "file"
        } else {
            "none"
        });
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
        cmd.current_dir(&self.home)
            .stdin(Stdio::null())
            .stdout(self.log_file("setup.log"))
            .stderr(self.log_file("setup.log"));
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        *self.setup.lock().unwrap() = Some(cmd.spawn().map_err(|e| e.to_string())?);
        Ok(())
    }

    /// Store the fal key: Keychain first, else `$FALOS_HOME/fal.env` (0600). Restarts the host so
    /// the runtime picks it up.
    pub fn save_key(&self, key: &str) -> Result<&'static str, String> {
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) || key.len() > 512 {
            return Err("that doesn't look like a fal key".into());
        }
        let stored = match keychain_entry().and_then(|e| e.set_password(key).map_err(|e| e.to_string())) {
            Ok(()) => {
                // The Keychain wins; drop an older plain-text copy in our home.
                let _ = std::fs::remove_file(self.home.join("fal.env"));
                "keychain"
            }
            Err(_) => {
                write_private(&self.home.join("fal.env"), &format!("FAL_KEY={key}\n"))?;
                "file"
            }
        };
        self.restart()?;
        Ok(stored)
    }

    pub fn forget_key(&self) -> Result<(), String> {
        if let Ok(e) = keychain_entry() {
            let _ = e.delete_credential();
        }
        let _ = std::fs::remove_file(self.home.join("fal.env"));
        self.restart()
    }
}

fn keychain_entry() -> Result<keyring::Entry, String> {
    if std::env::var_os("FALOS_NO_KEYCHAIN").is_some() {
        return Err("keychain disabled".into());
    }
    let service = std::env::var("FALOS_KEYCHAIN_SERVICE").unwrap_or_else(|_| KEYCHAIN_SERVICE.into());
    keyring::Entry::new(&service, KEYCHAIN_ACCOUNT).map_err(|e| e.to_string())
}

fn keychain_get() -> Option<String> {
    keychain_entry().ok()?.get_password().ok().filter(|k| !k.trim().is_empty())
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
