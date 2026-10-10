//! Register the host's MCP endpoint in agent clients' own config files (the config file is
//! the source of truth). Edits are key-order/format preserving and idempotent.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde_json::{Map, Value as Json};

pub const SERVER_NAME: &str = "fal-os";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Client {
    Claude,
    Codex,
    Cursor,
    Gemini,
    Opencode,
}

impl Client {
    pub fn config_path(self, home: &Path) -> PathBuf {
        match self {
            Client::Claude => home.join(".claude.json"),
            Client::Codex => home.join(".codex/config.toml"),
            Client::Cursor => home.join(".cursor/mcp.json"),
            Client::Gemini => home.join(".gemini/settings.json"),
            // `opencode.jsonc` when the user has one (edited only if it has no comments).
            Client::Opencode => {
                let dir = home.join(".config/opencode");
                let jsonc = dir.join("opencode.jsonc");
                if jsonc.exists() {
                    jsonc
                } else {
                    dir.join("opencode.json")
                }
            }
        }
    }
}

pub struct Outcome {
    pub path: PathBuf,
    pub before: String,
    pub after: String,
}

impl Outcome {
    pub fn changed(&self) -> bool {
        self.before != self.after
    }
}

/// Compute (and unless `dry_run`, write) the updated config for `client`.
pub fn register(
    client: Client,
    home: &Path,
    url: &str,
    token: &str,
    dry_run: bool,
) -> Result<Outcome> {
    let path = client.config_path(home);
    let before = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let auth = format!("Bearer {token}");
    let after = match client {
        Client::Codex => update_toml(&before, url, &auth)?,
        Client::Opencode => update_opencode(&before, url, &auth).with_context(|| {
            format!("{} is not plain JSON (comments?); edit it by hand", path.display())
        })?,
        _ => update_json(client, &before, url, &auth)
            .with_context(|| format!("{} is not plain JSON; edit it by hand", path.display()))?,
    };
    let out = Outcome {
        path,
        before,
        after,
    };
    if !dry_run && out.changed() {
        write_preserving_mode(&out.path, &out.after)?;
    }
    Ok(out)
}

fn update_json(client: Client, before: &str, url: &str, auth: &str) -> Result<String> {
    let mut root: Json = if before.trim().is_empty() {
        Json::Object(Map::new())
    } else {
        serde_json::from_str(before)?
    };
    let Some(obj) = root.as_object_mut() else {
        bail!("top level is not an object");
    };
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Json::Object(Map::new()));
    let Some(servers) = servers.as_object_mut() else {
        bail!("`mcpServers` is not an object");
    };
    let entry = servers
        .entry(SERVER_NAME)
        .or_insert_with(|| Json::Object(Map::new()));
    if !entry.is_object() {
        *entry = Json::Object(Map::new());
    }
    let e = entry.as_object_mut().unwrap();
    let url_key = match client {
        Client::Claude => {
            e.insert("type".into(), "http".into());
            "url"
        }
        Client::Gemini => "httpUrl",
        _ => "url",
    };
    e.insert(url_key.into(), url.into());
    let headers = e
        .entry("headers")
        .or_insert_with(|| Json::Object(Map::new()));
    if !headers.is_object() {
        *headers = Json::Object(Map::new());
    }
    headers
        .as_object_mut()
        .unwrap()
        .insert("Authorization".into(), auth.into());
    let mut s = serde_json::to_string_pretty(&root)?;
    if before.is_empty() || before.ends_with('\n') {
        s.push('\n');
    }
    Ok(s)
}

/// OpenCode: `{"mcp": {"fal-os": {"type": "remote", "url", "enabled": true, "headers"}}}`.
fn update_opencode(before: &str, url: &str, auth: &str) -> Result<String> {
    let mut root: Json = if before.trim().is_empty() {
        serde_json::json!({"$schema": "https://opencode.ai/config.json"})
    } else {
        serde_json::from_str(before)?
    };
    let Some(obj) = root.as_object_mut() else {
        bail!("top level is not an object");
    };
    let servers = obj.entry("mcp").or_insert_with(|| Json::Object(Map::new()));
    let Some(servers) = servers.as_object_mut() else {
        bail!("`mcp` is not an object");
    };
    let entry = servers
        .entry(SERVER_NAME)
        .or_insert_with(|| Json::Object(Map::new()));
    if !entry.is_object() {
        *entry = Json::Object(Map::new());
    }
    let e = entry.as_object_mut().unwrap();
    e.insert("type".into(), "remote".into());
    e.insert("url".into(), url.into());
    e.entry("enabled").or_insert(Json::Bool(true));
    let headers = e
        .entry("headers")
        .or_insert_with(|| Json::Object(Map::new()));
    if !headers.is_object() {
        *headers = Json::Object(Map::new());
    }
    headers
        .as_object_mut()
        .unwrap()
        .insert("Authorization".into(), auth.into());
    let mut s = serde_json::to_string_pretty(&root)?;
    if before.is_empty() || before.ends_with('\n') {
        s.push('\n');
    }
    Ok(s)
}

/// Codex: `[mcp_servers.fal-os] url = "…"` + `http_headers = { Authorization = "Bearer …" }`.
fn update_toml(before: &str, url: &str, auth: &str) -> Result<String> {
    use toml_edit::{DocumentMut, InlineTable, Item, Table, value};
    let mut doc: DocumentMut = before.parse().context("parsing config.toml")?;
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        })
        .as_table_mut()
        .context("`mcp_servers` is not a table")?;
    let entry = servers
        .entry(SERVER_NAME)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .context("`mcp_servers.fal-os` is not a table")?;
    if entry.get("url").and_then(|v| v.as_str()) != Some(url) {
        entry["url"] = value(url);
    }
    let current = entry
        .get("http_headers")
        .and_then(|h| h.get("Authorization"))
        .and_then(|v| v.as_str());
    if current != Some(auth) {
        match entry
            .get_mut("http_headers")
            .and_then(|h| h.as_inline_table_mut())
        {
            Some(t) => {
                t.insert("Authorization", auth.into());
            }
            None => {
                let mut t = InlineTable::new();
                t.insert("Authorization", auth.into());
                entry["http_headers"] = value(t);
            }
        }
    }
    Ok(doc.to_string())
}

fn write_preserving_mode(path: &Path, content: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let perms = std::fs::metadata(path).ok().map(|m| m.permissions());
    let tmp = path.with_extension("falos-tmp");
    std::fs::write(&tmp, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // New files hold a token: keep them private.
        let p = perms.unwrap_or_else(|| std::fs::Permissions::from_mode(0o600));
        std::fs::set_permissions(&tmp, p)?;
    }
    #[cfg(not(unix))]
    let _ = perms;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// A compact diff: the changed middle section with up to 3 lines of context.
pub fn diff(before: &str, after: &str) -> String {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let max_suf = a.len().min(b.len()) - pre;
    let suf = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max_suf)
        .take_while(|(x, y)| x == y)
        .count();
    let ctx = 3;
    let mut out = format!("@@ line {} @@\n", pre.saturating_sub(ctx) + 1);
    for l in &a[pre.saturating_sub(ctx)..pre] {
        out.push_str(&format!("  {l}\n"));
    }
    for l in &a[pre..a.len() - suf] {
        out.push_str(&format!("- {l}\n"));
    }
    for l in &b[pre..b.len() - suf] {
        out.push_str(&format!("+ {l}\n"));
    }
    for l in a[a.len() - suf..].iter().take(ctx) {
        out.push_str(&format!("  {l}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "http://127.0.0.1:7317/mcp";

    #[test]
    fn json_clients_preserve_order_and_are_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude.json");
        std::fs::write(
            &path,
            "{\n  \"zeta\": 1,\n  \"mcpServers\": {\n    \"other\": {\"command\": \"x\"}\n  },\n  \"alpha\": true\n}\n",
        )
        .unwrap();
        let dry = register(Client::Claude, home.path(), URL, "t1", true).unwrap();
        assert!(dry.changed());
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"zeta\": 1,\n  \"mcpServers\"")
        );
        assert!(
            !std::fs::read_to_string(&path).unwrap().contains("fal-os"),
            "dry run wrote"
        );
        assert!(diff(&dry.before, &dry.after).contains("+       \"type\": \"http\""));

        let o = register(Client::Claude, home.path(), URL, "t1", false).unwrap();
        let v: Json = serde_json::from_str(&o.after).unwrap();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["zeta", "mcpServers", "alpha"]);
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["fal-os"]["type"], "http");
        assert_eq!(v["mcpServers"]["fal-os"]["url"], URL);
        assert_eq!(
            v["mcpServers"]["fal-os"]["headers"]["Authorization"],
            "Bearer t1"
        );
        assert!(
            !register(Client::Claude, home.path(), URL, "t1", false)
                .unwrap()
                .changed()
        );

        let g = register(Client::Gemini, home.path(), URL, "t1", false).unwrap();
        assert!(g.after.contains("\"httpUrl\""));
        let c = register(Client::Cursor, home.path(), URL, "t1", false).unwrap();
        assert!(c.path.ends_with(".cursor/mcp.json") && c.after.contains("\"url\""));
    }

    #[test]
    fn codex_toml_is_format_preserving() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".codex/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let orig = "# my settings\nmodel = \"gpt-5\"   # favourite\n\n[mcp_servers.docs]\ncommand = \"docs-mcp\"\n";
        std::fs::write(&path, orig).unwrap();
        let o = register(Client::Codex, home.path(), URL, "t1", false).unwrap();
        assert!(o.after.starts_with(orig), "{}", o.after);
        assert!(o.after.contains("[mcp_servers.fal-os]\nurl = \"http://127.0.0.1:7317/mcp\"\nhttp_headers = { Authorization = \"Bearer t1\" }"), "{}", o.after);
        assert!(
            !register(Client::Codex, home.path(), URL, "t1", false)
                .unwrap()
                .changed()
        );
        // Token rotation updates in place.
        let o = register(Client::Codex, home.path(), URL, "t2", false).unwrap();
        assert!(o.after.contains("Bearer t2") && !o.after.contains("Bearer t1"));
    }

    #[test]
    fn opencode_json_and_jsonc() {
        let home = tempfile::tempdir().unwrap();
        let o = register(Client::Opencode, home.path(), URL, "t1", false).unwrap();
        assert!(o.path.ends_with(".config/opencode/opencode.json"));
        let v: Json = serde_json::from_str(&o.after).unwrap();
        assert_eq!(v["mcp"]["fal-os"]["type"], "remote");
        assert_eq!(v["mcp"]["fal-os"]["url"], URL);
        assert_eq!(v["mcp"]["fal-os"]["enabled"], true);
        assert_eq!(v["mcp"]["fal-os"]["headers"]["Authorization"], "Bearer t1");
        assert!(!register(Client::Opencode, home.path(), URL, "t1", false).unwrap().changed());
        // A .jsonc wins; with comments it is refused and left alone.
        let jsonc = home.path().join(".config/opencode/opencode.jsonc");
        std::fs::write(&jsonc, "{ // mine\n}").unwrap();
        assert!(register(Client::Opencode, home.path(), URL, "t1", false).is_err());
        assert_eq!(std::fs::read_to_string(&jsonc).unwrap(), "{ // mine\n}");
        std::fs::write(&jsonc, "{\"theme\": \"x\"}\n").unwrap();
        let o = register(Client::Opencode, home.path(), URL, "t1", false).unwrap();
        assert!(o.path.ends_with("opencode.jsonc") && o.after.starts_with("{\n  \"theme\""));
    }

    #[test]
    fn refuses_jsonc() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".cursor/mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ // comment\n}").unwrap();
        assert!(register(Client::Cursor, home.path(), URL, "t", false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ // comment\n}");
    }
}
