//! Library queries: one `Query` shape for the UI (`library.query`) and agents
//! (`search_assets`). Filters and smart rules compile to SQL over the index; text uses FTS5
//! (prompt, caption, tags, name, model); `color` ranks by Lab distance to the palettes;
//! `similar_to` ranks by the tiny 8×8 vectors (fal semantic results are merged in by the
//! caller when the asset is on fal).
//!
//! Smart rule JSON: `{"all"|"any": [rule…]}` or `{"field", "op", "value"}` with fields
//! `kind source role model tag favorite created w h duration orientation project collection
//! text local_only synced` and ops `eq ne in contains gt gte lt lte`.

use std::collections::HashMap;

use anyhow::{Result, bail};
use rusqlite::Connection;
use rusqlite::types::Value as Sql;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use super::thumbs;

#[derive(Deserialize, Default, Clone, Debug, schemars::JsonSchema)]
#[serde(default)]
pub struct Query {
    /// Words matched against prompt, caption, tags, name and model (prefix match).
    pub text: Option<String>,
    /// Rank by visual similarity to this asset (sha or sha prefix, or a node id).
    pub similar_to: Option<String>,
    /// `#rrggbb`: rank by closeness to this color.
    pub color: Option<String>,
    /// import | generate | derive | render
    pub source: Option<Vec<String>>,
    /// input | candidate | wip | final | rejected (node role of a placement)
    pub role: Option<Vec<String>>,
    /// image | video | audio | model3d | other
    pub kind: Option<Vec<String>>,
    /// Project id, or "*" (default) for everything incl. the fal library.
    pub project: Option<String>,
    pub favorite: Option<bool>,
    /// Collection id or name.
    pub collection: Option<String>,
    pub tag: Option<Vec<String>>,
    /// Model / endpoint substring.
    pub model: Option<String>,
    /// Created at or after (ms).
    pub after: Option<i64>,
    pub before: Option<i64>,
    /// Exactly these assets (e.g. `library.show`).
    pub shas: Option<Vec<String>>,
    pub local_only: Option<bool>,
    /// On fal (has an assetRecordId).
    pub synced: Option<bool>,
    /// Smart rule JSON (see module docs).
    pub rule: Option<Json>,
    /// newest (default) | oldest | name | relevance
    pub sort: Option<String>,
    pub cursor: Option<String>,
    /// Default 200, max 10000.
    pub limit: Option<usize>,
    /// Also ask fal semantic search for `text` (default true when fal is connected).
    pub semantic: Option<bool>,
}

struct Sqlb {
    wh: Vec<String>,
    params: Vec<Sql>,
}

impl Sqlb {
    fn p(&mut self, v: impl Into<Sql>) -> String {
        self.params.push(v.into());
        format!("?{}", self.params.len())
    }
    fn list(&mut self, v: &[String]) -> String {
        let s = serde_json::to_string(v).unwrap_or_default();
        format!("(SELECT value FROM json_each({}))", self.p(s))
    }
}

const ROLE_EXPR: &str = "coalesce(p.role, CASE a.source WHEN 'import' THEN 'input' ELSE 'candidate' END)";

/// FTS5 query from free text: every word as a quoted prefix term (AND).
pub fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .take(12)
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

fn vals(v: &Json) -> Vec<String> {
    match v {
        Json::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect(),
        Json::String(s) => vec![s.clone()],
        other => vec![other.to_string()],
    }
}

fn cond(b: &mut Sqlb, field: &str, op: &str, value: &Json, project: Option<&str>) -> Result<String> {
    let num_cmp = |b: &mut Sqlb, col: &str| -> Result<String> {
        let o = match op {
            "gt" => ">",
            "gte" => ">=",
            "lt" => "<",
            "lte" => "<=",
            "eq" => "=",
            "ne" => "!=",
            _ => bail!("op `{op}` doesn't apply to `{field}`"),
        };
        let v = value.as_f64().ok_or_else(|| anyhow::anyhow!("`{field}` needs a number"))?;
        Ok(format!("{col} {o} {}", b.p(v)))
    };
    let set = |b: &mut Sqlb, col: &str| -> String {
        let l = b.list(&vals(value));
        match op {
            "ne" => format!("{col} NOT IN {l}"),
            _ => format!("{col} IN {l}"),
        }
    };
    Ok(match field {
        "kind" | "source" => set(b, &format!("a.{field}")),
        "model" => match op {
            "eq" => format!("a.model = {}", b.p(vals(value).join(""))),
            _ => format!("a.model LIKE {}", b.p(format!("%{}%", vals(value).join("")))),
        },
        "tag" => {
            let l = b.list(&vals(value).iter().map(|t| t.to_lowercase()).collect::<Vec<_>>());
            let e = format!("EXISTS(SELECT 1 FROM tags t WHERE t.sha=a.sha AND t.tag IN {l})");
            if op == "ne" { format!("NOT {e}") } else { e }
        }
        "favorite" | "local_only" => format!("a.{field} = {}", if value.as_bool().unwrap_or(true) { 1 } else { 0 }),
        "synced" => if value.as_bool().unwrap_or(true) { "a.fal_id IS NOT NULL".into() } else { "a.fal_id IS NULL".into() },
        "created" | "w" | "h" | "duration" => num_cmp(b, &format!("a.{field}"))?,
        "orientation" => match value.as_str().unwrap_or("") {
            "landscape" => "a.w > a.h".into(),
            "portrait" => "a.w < a.h".into(),
            "square" => "a.w = a.h".into(),
            o => bail!("orientation `{o}`: landscape|portrait|square"),
        },
        "project" => {
            let l = b.list(&vals(value));
            let e = format!("a.sha IN (SELECT sha FROM asset_projects WHERE project IN {l} UNION SELECT sha FROM placements WHERE project IN {l})");
            if op == "ne" { format!("NOT ({e})") } else { e }
        }
        "collection" => {
            let v = vals(value).join("");
            let (p1, p2) = (b.p(v.clone()), b.p(v));
            format!("a.sha IN (SELECT i.sha FROM collection_items i JOIN collections c ON c.id=i.collection WHERE c.id={p1} OR c.fal_id={p1} OR lower(c.name)=lower({p2}))")
        }
        "role" => {
            let l = b.list(&vals(value));
            let scope = match project {
                Some(pr) => format!(" AND p.project={}", b.p(pr.to_string())),
                None => String::new(),
            };
            format!("EXISTS(SELECT 1 FROM placements p WHERE p.sha=a.sha{scope} AND {ROLE_EXPR} IN {l})")
        }
        "text" => match fts_query(&vals(value).join(" ")) {
            Some(q) => format!("a.rowid IN (SELECT rowid FROM fts WHERE fts MATCH {})", b.p(q)),
            None => "1".into(),
        },
        f => bail!("unknown rule field `{f}`"),
    })
}

fn rule(b: &mut Sqlb, r: &Json, project: Option<&str>, depth: usize) -> Result<String> {
    if depth > 6 {
        bail!("rule nests too deep");
    }
    for (k, join) in [("all", " AND "), ("any", " OR ")] {
        if let Some(list) = r.get(k).and_then(Json::as_array) {
            let parts: Vec<String> = list.iter().map(|x| rule(b, x, project, depth + 1)).collect::<Result<_>>()?;
            return Ok(if parts.is_empty() { "1".into() } else { format!("({})", parts.join(join)) });
        }
    }
    let field = r["field"].as_str().ok_or_else(|| anyhow::anyhow!("rule needs `field` (or all/any)"))?;
    let op = r["op"].as_str().unwrap_or(if r["value"].is_array() { "in" } else { "eq" });
    cond(b, field, op, &r["value"], project)
}

/// The scoped project of a query (`None` = everything).
pub fn scope(q: &Query) -> Option<&str> {
    q.project.as_deref().filter(|p| !p.is_empty() && *p != "*")
}

/// Resolve a sha, sha prefix (≥ 6 chars) or `fal_…` key to a library key.
pub fn resolve_key(c: &Connection, key: &str) -> Option<String> {
    let k = key.trim().trim_start_matches("sha256:").to_ascii_lowercase();
    if k.is_empty() {
        return None;
    }
    if let Ok(s) = c.query_row("SELECT sha FROM assets WHERE sha=?", [&k], |r| r.get::<_, String>(0)) {
        return Some(s);
    }
    if k.len() >= 6 && k.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut st = c.prepare_cached("SELECT sha FROM assets WHERE sha >= ?1 AND sha < ?1 || 'g' LIMIT 2").ok()?;
        let v: Vec<String> = st.query_map([&k], |r| r.get(0)).ok()?.filter_map(|x| x.ok()).collect();
        if v.len() == 1 {
            return v.into_iter().next();
        }
    }
    c.query_row("SELECT sha FROM assets WHERE fal_id=?", [key.trim().trim_start_matches("fal_")], |r| r.get(0)).ok()
}

/// A node id → the asset it shows (in `project`, else anywhere).
pub fn key_of_node(c: &Connection, node: &str, project: Option<&str>) -> Option<String> {
    c.query_row(
        "SELECT sha FROM placements WHERE node=?1 AND (?2 IS NULL OR project=?2) LIMIT 1",
        rusqlite::params![node, project],
        |r| r.get(0),
    )
    .ok()
}

pub struct Page {
    pub keys: Vec<String>,
    pub next: Option<String>,
    pub total: usize,
}

/// Run a query → ordered keys of one page.
pub fn run(c: &Connection, q: &Query) -> Result<Page> {
    let project = scope(q);
    let mut b = Sqlb { wh: vec![], params: vec![] };
    let mut conds: Vec<(String, String, Json)> = vec![];
    if let Some(p) = project {
        conds.push(("project".into(), "in".into(), json!([p])));
    }
    for (f, v) in [("source", &q.source), ("role", &q.role), ("kind", &q.kind), ("tag", &q.tag)] {
        if let Some(v) = v.as_ref().filter(|v| !v.is_empty()) {
            conds.push((f.into(), "in".into(), json!(v)));
        }
    }
    if let Some(f) = q.favorite {
        conds.push(("favorite".into(), "eq".into(), json!(f)));
    }
    if let Some(f) = q.local_only {
        conds.push(("local_only".into(), "eq".into(), json!(f)));
    }
    if let Some(f) = q.synced {
        conds.push(("synced".into(), "eq".into(), json!(f)));
    }
    if let Some(m) = q.model.as_ref().filter(|m| !m.is_empty()) {
        conds.push(("model".into(), "contains".into(), json!(m)));
    }
    if let Some(c) = q.collection.as_ref().filter(|m| !m.is_empty()) {
        conds.push(("collection".into(), "eq".into(), json!(c)));
    }
    if let Some(t) = q.after {
        conds.push(("created".into(), "gte".into(), json!(t)));
    }
    if let Some(t) = q.before {
        conds.push(("created".into(), "lt".into(), json!(t)));
    }
    for (f, op, v) in &conds {
        let s = cond(&mut b, f, op, v, project)?;
        b.wh.push(s);
    }
    if let Some(r) = &q.rule {
        let s = rule(&mut b, r, project, 0)?;
        b.wh.push(s);
    }
    if let Some(shas) = &q.shas {
        let keys: Vec<String> = shas.iter().filter_map(|s| resolve_key(c, s)).collect();
        let l = b.list(&keys);
        b.wh.push(format!("a.sha IN {l}"));
    }
    let fts = q.text.as_deref().and_then(fts_query);
    let mut from = "assets a".to_string();
    if let Some(f) = &fts {
        from = format!("assets a JOIN (SELECT rowid AS r, bm25(fts, 4.0, 2.0, 3.0, 3.0, 1.0) AS score FROM fts WHERE fts MATCH {}) f ON f.r=a.rowid", b.p(f.clone()));
    }
    let wh = if b.wh.is_empty() { "1".into() } else { b.wh.join(" AND ") };
    let sort = q.sort.as_deref().unwrap_or(if fts.is_some() { "relevance" } else { "newest" });
    let order = match sort {
        "oldest" => "a.created ASC, a.rowid ASC",
        "name" => "lower(coalesce(a.name, a.prompt, '')) ASC, a.created DESC",
        "relevance" if fts.is_some() => "f.score ASC, a.created DESC",
        _ => "a.created DESC, a.rowid DESC",
    };
    let sql = format!("SELECT a.sha, a.palette, a.tiny FROM {from} WHERE {wh} ORDER BY {order}");
    let mut st = c.prepare(&sql)?;
    let rows: Vec<(String, Option<String>, Option<Vec<u8>>)> =
        st.query_map(rusqlite::params_from_iter(b.params.iter()), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.filter_map(|r| r.ok()).collect();
    let mut keys: Vec<String>;
    if let Some(hex) = q.color.as_deref().and_then(thumbs::parse_hex) {
        let target = thumbs::srgb_to_lab(hex);
        let mut scored: Vec<(f32, String)> = rows
            .into_iter()
            .filter_map(|(k, pal, _)| {
                let pal: Vec<Json> = serde_json::from_str(pal.as_deref()?).ok()?;
                let d = pal
                    .iter()
                    .filter_map(|e| {
                        let rgb = thumbs::parse_hex(e["hex"].as_str()?)?;
                        let w = e["w"].as_f64().unwrap_or(0.0) as f32;
                        Some(thumbs::delta_e(target, thumbs::srgb_to_lab(rgb)) * (1.0 - 0.5 * w.min(1.0)))
                    })
                    .fold(f32::MAX, f32::min);
                (d < 28.0).then_some((d, k))
            })
            .collect();
        scored.sort_by(|a, b| a.0.total_cmp(&b.0));
        keys = scored.into_iter().map(|(_, k)| k).collect();
    } else if let Some(sim) = q.similar_to.as_deref() {
        let key = resolve_key(c, sim).or_else(|| key_of_node(c, sim, project));
        let Some(key) = key else { bail!("similar_to: no asset `{sim}`") };
        let tiny: Option<Vec<u8>> = c.query_row("SELECT tiny FROM assets WHERE sha=?", [&key], |r| r.get(0)).ok().flatten();
        let Some(t) = tiny else {
            return Ok(Page { keys: vec![], next: None, total: 0 });
        };
        let mut scored: Vec<(f32, String)> = rows.into_iter().filter(|r| r.0 != key).filter_map(|(k, _, ti)| Some((thumbs::tiny_distance(&t, &ti?), k))).collect();
        scored.sort_by(|a, b| a.0.total_cmp(&b.0));
        keys = std::iter::once(key).chain(scored.into_iter().filter(|s| s.0 < 70.0).map(|(_, k)| k)).collect();
    } else {
        keys = rows.into_iter().map(|r| r.0).collect();
    }
    let total = keys.len();
    let offset: usize = q.cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
    let limit = q.limit.unwrap_or(200).clamp(1, 10_000);
    let next = (offset + limit < total).then(|| (offset + limit).to_string());
    keys = keys.into_iter().skip(offset).take(limit).collect();
    Ok(Page { keys, next, total })
}

const ROLE_RANK: [&str; 5] = ["final", "wip", "candidate", "input", "rejected"];

/// Contract items (`library.results`) for keys, in order.
pub fn items(c: &Connection, keys: &[String], project: Option<&str>) -> Result<Vec<Json>> {
    if keys.is_empty() {
        return Ok(vec![]);
    }
    let list = serde_json::to_string(keys)?;
    let mut tags: HashMap<String, Vec<String>> = HashMap::new();
    let mut st = c.prepare_cached("SELECT sha, tag FROM tags WHERE sha IN (SELECT value FROM json_each(?)) ORDER BY tag")?;
    for r in st.query_map([&list], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.flatten() {
        tags.entry(r.0).or_default().push(r.1);
    }
    let mut projects: HashMap<String, Vec<String>> = HashMap::new();
    let mut st = c.prepare_cached("SELECT sha, project FROM asset_projects WHERE sha IN (SELECT value FROM json_each(?1)) UNION SELECT sha, project FROM placements WHERE sha IN (SELECT value FROM json_each(?1))")?;
    for r in st.query_map([&list], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.flatten() {
        projects.entry(r.0).or_default().push(r.1);
    }
    let mut roles: HashMap<String, Vec<(String, Option<String>, String)>> = HashMap::new();
    let mut st = c.prepare_cached("SELECT sha, project, role, node FROM placements WHERE sha IN (SELECT value FROM json_each(?))")?;
    for r in st.query_map([&list], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?)))?.flatten() {
        roles.entry(r.0).or_default().push((r.1, r.2, r.3));
    }
    let mut st = c.prepare_cached(
        "SELECT sha,kind,mime,w,h,duration,source,prompt,model,caption,palette,favorite,fal_id,fal_url,fal_synced,created,name,local_only,path,bytes,thumb,fal_request_id,lineage
         FROM assets WHERE sha IN (SELECT value FROM json_each(?))",
    )?;
    let mut by: HashMap<String, Json> = HashMap::new();
    let rows = st.query_map([&list], |r| {
        let sha: String = r.get(0)?;
        let source: String = r.get(6)?;
        let palette: Option<String> = r.get(10)?;
        let fal_id: Option<String> = r.get(12)?;
        let fal_url: Option<String> = r.get(13)?;
        let path: Option<String> = r.get(18)?;
        let kind: String = r.get(1)?;
        let thumb: i64 = r.get(20)?;
        Ok((sha, kind, source, palette, fal_id, fal_url, path, thumb, r.get::<_, Option<String>>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, Option<i64>>(4)?, r.get::<_, Option<f64>>(5)?,
            r.get::<_, Option<String>>(7)?, r.get::<_, Option<String>>(8)?, r.get::<_, Option<String>>(9)?, r.get::<_, i64>(11)?, r.get::<_, i64>(14)?, r.get::<_, i64>(15)?,
            r.get::<_, Option<String>>(16)?, r.get::<_, i64>(17)?, r.get::<_, Option<i64>>(19)?, r.get::<_, Option<String>>(21)?, r.get::<_, Option<String>>(22)?))
    })?;
    for row in rows.flatten() {
        let (sha, kind, source, palette, fal_id, fal_url, path, thumb, mime, w, h, duration, prompt, model, caption, favorite, synced, created, name, local_only, bytes, _req, lineage) = row;
        // The doc model's one-line lineage summary (op, recipe brief, parents, actor).
        let lineage = lineage
            .and_then(|l| serde_json::from_str::<falos_doc::Lineage>(&l).ok())
            .filter(|l| l.kind != falos_doc::LineageKind::Import)
            .map(|l| l.summary(|p| falos_doc::Asset::short_id(&p.asset)));
        let default_role = if source == "import" { "input" } else { "candidate" };
        let role = roles.get(&sha).and_then(|ps| {
            ps.iter()
                .filter(|(p, _, _)| project.is_none_or(|x| x == p))
                .map(|(_, r, _)| r.clone().unwrap_or_else(|| default_role.to_string()))
                .min_by_key(|r| ROLE_RANK.iter().position(|x| x == r).unwrap_or(9))
        });
        let nodes: Vec<Json> = roles.get(&sha).map(|ps| ps.iter().filter(|(p, _, _)| project.is_none_or(|x| x == p)).map(|(p, _, n)| json!({"project": p, "node": n})).collect()).unwrap_or_default();
        let palette: Option<Vec<String>> = palette.and_then(|p| serde_json::from_str::<Vec<Json>>(&p).ok()).map(|v| v.iter().filter_map(|e| e["hex"].as_str().map(str::to_string)).collect());
        let local = path.as_deref().is_some_and(|p| std::path::Path::new(p).is_file());
        let src = if local && sha.len() == 64 { Some(format!("/assets/{sha}")) } else { fal_url.clone() };
        let mut item = json!({
            "sha": sha, "kind": kind, "mime": mime, "w": w, "h": h, "duration": duration,
            "source": source, "role": role, "prompt": prompt, "model": model, "caption": caption,
            "palette": palette, "favorite": favorite != 0,
            "fal": if fal_id.is_some() || fal_url.is_some() { json!({"assetRecordId": fal_id, "url": fal_url, "synced": synced != 0}) } else { Json::Null },
            "projects": projects.remove(&sha).unwrap_or_default(),
            "thumb": format!("/thumbs/{sha}-256.webp"), "thumb_lg": format!("/thumbs/{sha}-1024.webp"),
            "created": created,
            "name": name, "tags": tags.remove(&sha).unwrap_or_default(), "local_only": local_only != 0,
            "local": local, "src": src, "bytes": bytes, "nodes": nodes, "lineage": lineage,
            "thumb_state": match thumb { 1 => "ok", 2 => "failed", 3 => "none", _ => "pending" },
        });
        if kind == "video" {
            item["strip"] = json!(format!("/thumbs/{sha}-strip.webp"));
        }
        by.insert(item["sha"].as_str().unwrap_or_default().to_string(), item);
    }
    Ok(keys.iter().filter_map(|k| by.remove(k)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db;

    fn lib() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        db::init(&c).unwrap();
        let mk = |sha: &str, kind: &str, source: &str, prompt: &str, created: i64| db::Ingested {
            sha: sha.into(),
            kind: kind.into(),
            source: Some(source.into()),
            prompt: Some(prompt.into()),
            created: Some(created),
            w: Some(100),
            h: Some(50),
            ..Default::default()
        };
        for (i, (k, s, p)) in [("image", "import", "red fox reference"), ("video", "generate", "fox running in snow"), ("image", "generate", "blue whale")].iter().enumerate() {
            let sha = format!("{i}").repeat(64);
            db::upsert_ingested(&c, &mk(&sha, k, s, p, i as i64), 0).unwrap();
            c.execute("INSERT INTO asset_projects VALUES(?, 'demo')", [&sha]).unwrap();
        }
        c.execute("UPDATE assets SET palette='[{\"hex\":\"#ff0000\",\"w\":0.8}]' WHERE sha=?", [&"0".repeat(64)]).unwrap();
        c.execute("INSERT INTO placements VALUES('demo','main','n1',?, 'final', 'hero')", [&"2".repeat(64)]).unwrap();
        c
    }

    #[test]
    fn filters_text_color_rule() {
        let c = lib();
        let q = |q: Query| run(&c, &q).unwrap().keys;
        assert_eq!(q(Query { text: Some("fox".into()), ..Default::default() }).len(), 2);
        assert_eq!(q(Query { text: Some("fox".into()), kind: Some(vec!["video".into()]), ..Default::default() }), vec!["1".repeat(64)]);
        assert_eq!(q(Query { color: Some("#f01010".into()), ..Default::default() }), vec!["0".repeat(64)]);
        assert_eq!(q(Query { role: Some(vec!["final".into()]), project: Some("demo".into()), ..Default::default() }), vec!["2".repeat(64)]);
        assert_eq!(q(Query { source: Some(vec!["generate".into()]), ..Default::default() }), vec!["2".repeat(64), "1".repeat(64)]);
        let r = json!({"any": [{"field": "kind", "value": "video"}, {"field": "text", "value": "whale"}]});
        assert_eq!(q(Query { rule: Some(r), sort: Some("oldest".into()), ..Default::default() }), vec!["1".repeat(64), "2".repeat(64)]);
        let p = run(&c, &Query { limit: Some(2), ..Default::default() }).unwrap();
        assert_eq!((p.keys.len(), p.next.as_deref(), p.total), (2, Some("2"), 3));
        let it = items(&c, &p.keys, Some("demo")).unwrap();
        assert_eq!(it[0]["role"], "final");
        assert_eq!(it[0]["thumb"], format!("/thumbs/{}-256.webp", "2".repeat(64)));
        assert_eq!(resolve_key(&c, "222222"), Some("2".repeat(64)));
    }
}
