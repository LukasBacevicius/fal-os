//! Line-oriented, id-keyed text projection of a canvas (spec: `FORMAT.md`).

use std::collections::HashMap;
use std::fmt::Write;

use loro::{TreeID, TreeParentId};
use serde_json::Value as Json;

use crate::canvas::Tx;
use crate::model::{Actor, Anchor, Annotation, Camera, Node, NodeKind, Region, Status};
use crate::{Canvas, EditResult, Error, Props, Result, Version};

#[derive(Clone, Debug)]
pub struct ReadOpts {
    /// Only this node's subtree (the node's row included).
    pub root: Option<String>,
    /// Max nesting depth below the top rows; deeper children are summarized as `more(n)`.
    pub depth: Option<usize>,
    /// Keep nodes whose type (`image`, `style`, `style:lut`), id, or name (case-insensitive
    /// substring) matches; their ancestors are kept for context.
    pub filter: Option<String>,
    pub annotations: bool,
    /// Print `by(@…)` / `upd(@…)` and message authors.
    pub attribution: bool,
}

impl Default for ReadOpts {
    fn default() -> Self {
        Self {
            root: None,
            depth: None,
            filter: None,
            annotations: true,
            attribution: true,
        }
    }
}

// ---- printing ---------------------------------------------------------------------------

fn num(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

fn nums(v: &[f64]) -> String {
    v.iter().map(|f| num(*f)).collect::<Vec<_>>().join(",")
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn is_bare(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.:/@-".contains(c))
        && matches!(parse_scalar(s), Ok(Json::String(_)))
}

fn value(v: &Json) -> String {
    let scalar = |v: &Json| !matches!(v, Json::Array(_) | Json::Object(_));
    match v {
        Json::Number(n) => n
            .as_i64()
            .map_or_else(|| num(n.as_f64().unwrap_or(0.0)), |i| i.to_string()),
        Json::String(s) if is_bare(s) => s.clone(),
        Json::String(s) => quote(s),
        Json::Array(a) if a.len() >= 2 && a.iter().all(scalar) => {
            a.iter().map(value).collect::<Vec<_>>().join(",")
        }
        _ => v.to_string(),
    }
}

fn prop(k: &str, v: &Json) -> String {
    match v {
        Json::Bool(true) => k.to_string(),
        _ => format!("{k}({})", value(v)),
    }
}

fn region(r: &Region) -> String {
    match r {
        Region::Point { x, y } => format!("pt({})", nums(&[*x, *y])),
        Region::Box { x, y, w, h } => format!("box({})", nums(&[*x, *y, *w, *h])),
        Region::Polygon { points } => format!("poly({})", nums(&points.concat())),
        Region::Mask { asset } => format!("mask({})", value(&Json::String(asset.clone()))),
    }
}

/// An anchor in the text syntax, e.g. `box(0.1,0.2,0.4,0.3)` or `frame(4.2) pt(0.5,0.5)`.
pub fn anchor_to_text(a: &Anchor) -> String {
    anchor(a)
}

/// Parses an anchor written in the text syntax (`pt(..)`, `box(..)`, `frame(t) box(..)`, …).
pub fn parse_anchor_text(s: &str) -> Result<Anchor, String> {
    let mut parts = vec![];
    for t in tokenize(s)? {
        match t {
            Tok::Call(n, a) if ANCHOR_CALLS.contains(&n.as_str()) => parts.push((n, a)),
            Tok::Call(n, _) | Tok::Word(n) => {
                return Err(format!("`{n}` is not part of an anchor"));
            }
            Tok::Str(_) => return Err("unexpected string in anchor".into()),
        }
    }
    parse_anchor(&parts)?.ok_or_else(|| "empty anchor".to_string())
}

fn anchor(a: &Anchor) -> String {
    let with = |head: String, r: &Option<Region>| match r {
        Some(r) => format!("{head} {}", region(r)),
        None => head,
    };
    match a {
        Anchor::Point { x, y } => region(&Region::Point { x: *x, y: *y }),
        Anchor::Box { x, y, w, h } => region(&Region::Box {
            x: *x,
            y: *y,
            w: *w,
            h: *h,
        }),
        Anchor::Polygon { points } => region(&Region::Polygon {
            points: points.clone(),
        }),
        Anchor::Mask { asset } => region(&Region::Mask {
            asset: asset.clone(),
        }),
        Anchor::Time { t } => format!("t({})", num(*t)),
        Anchor::Range { t0, t1 } => format!("range({})", nums(&[*t0, *t1])),
        Anchor::Frame { t, region } => with(format!("frame({})", num(*t)), region),
        Anchor::TextRange { start, end } => format!("chars({start},{end})"),
        Anchor::Page { n, region } => with(format!("page({n})"), region),
        Anchor::Point3d { p, camera: c } => format!(
            "pt3({}) cam({})",
            nums(p),
            nums(&[
                c.pos[0],
                c.pos[1],
                c.pos[2],
                c.target[0],
                c.target[1],
                c.target[2],
                c.fov
            ])
        ),
    }
}

fn node_row(n: &Node, hidden: usize, o: &ReadOpts) -> String {
    let mut parts = vec![n.id.clone(), n.kind.to_string()];
    if !n.name.is_empty() {
        parts.push(quote(&n.name));
    }
    if n.x != 0.0 || n.y != 0.0 {
        parts.push(format!("p({})", nums(&[n.x, n.y])));
    }
    if n.w != 0.0 || n.h != 0.0 {
        parts.push(format!("s({})", nums(&[n.w, n.h])));
    }
    if n.rot != 0.0 {
        parts.push(format!("r({})", num(n.rot)));
    }
    if n.locked {
        parts.push("locked".into());
    }
    let known: Vec<&str> = n.kind.props().iter().map(|(k, _)| *k).collect();
    for k in &known {
        if let Some(v) = n.props.get(*k) {
            parts.push(prop(k, v));
        }
    }
    for (k, v) in n.props.iter().filter(|(k, _)| !known.contains(&k.as_str())) {
        parts.push(prop(k, v));
    }
    if let Some(t) = n.text.as_deref().filter(|t| !t.is_empty()) {
        parts.push(format!("text({})", quote(t)));
    }
    if hidden > 0 {
        parts.push(format!("more({hidden})"));
    }
    if o.attribution && !n.created_by.is_empty() {
        parts.push(format!("by(@{})", n.created_by));
        if !n.updated_by.is_empty() && n.updated_by != n.created_by {
            parts.push(format!("upd(@{})", n.updated_by));
        }
    }
    parts.join(" ")
}

fn ann_rows(a: &Annotation, depth: usize, o: &ReadOpts, out: &mut String) {
    let pad = "  ".repeat(depth);
    let mut parts = vec![
        "!".to_string(),
        a.id.clone(),
        a.status.as_str().into(),
        anchor(&a.anchor),
    ];
    if let Some(x) = &a.assignee {
        parts.push(format!("to(@{x})"));
    }
    if !a.outcome.is_empty() {
        let ids: Vec<Json> = a.outcome.iter().map(|s| Json::String(s.clone())).collect();
        parts.push(format!("out({})", value(&Json::Array(ids))));
    }
    if o.attribution && !a.author.is_empty() {
        parts.push(format!("by(@{})", a.author));
    }
    let _ = writeln!(out, "{pad}{}", parts.join(" "));
    for m in &a.thread {
        let who = if o.attribution {
            format!("@{} ", m.author)
        } else {
            String::new()
        };
        let _ = writeln!(out, "{pad}  > {who}{}", quote(&m.text));
    }
}

fn matches(n: &Node, f: &str) -> bool {
    let kind = n.kind.to_string();
    kind == f
        || kind.split(':').next() == Some(f)
        || n.id == f
        || n.name.to_lowercase().contains(&f.to_lowercase())
}

impl Canvas {
    /// Render the canvas (or a subtree) in the text format. Deterministic.
    pub fn read(&self, o: &ReadOpts) -> Result<String> {
        let mut anns: HashMap<String, Vec<Annotation>> = HashMap::new();
        if o.annotations {
            for a in self.annotations() {
                anns.entry(a.node_id.clone()).or_default().push(a);
            }
        }
        let roots = match &o.root {
            Some(id) => vec![
                self.tree_id(id)
                    .ok_or_else(|| Error::UnknownNode(id.clone()))?,
            ],
            None => self.children_of(TreeParentId::Root),
        };
        let mut out = String::new();
        for t in roots {
            out.extend(self.render(t, 0, o, &anns));
        }
        Ok(out)
    }

    fn render(
        &self,
        t: TreeID,
        depth: usize,
        o: &ReadOpts,
        anns: &HashMap<String, Vec<Annotation>>,
    ) -> Option<String> {
        let n = self.read_node(t)?;
        let mut body = String::new();
        let mut hidden = 0;
        if o.depth.is_some_and(|d| depth >= d) {
            let mut stack = self.children_of(TreeParentId::Node(t));
            while let Some(c) = stack.pop() {
                hidden += 1;
                stack.extend(self.children_of(TreeParentId::Node(c)));
            }
        } else {
            for c in self.children_of(TreeParentId::Node(t)) {
                body.extend(self.render(c, depth + 1, o, anns));
            }
        }
        if body.is_empty() && o.filter.as_deref().is_some_and(|f| !matches(&n, f)) {
            return None;
        }
        let mut s = format!("{}{}\n", "  ".repeat(depth), node_row(&n, hidden, o));
        for a in anns.get(&n.id).into_iter().flatten() {
            ann_rows(a, depth + 1, o, &mut s);
        }
        s.push_str(&body);
        Some(s)
    }

    /// Apply an edit in the text format as one transaction (one undo step) by `actor`.
    pub fn apply_edit(
        &mut self,
        text: &str,
        actor: &Actor,
        expected: Option<&Version>,
    ) -> Result<EditResult> {
        let rows = parse(text)?;
        self.transact(actor, expected, |tx| exec(tx, &rows))
    }
}

// ---- parsing ----------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Call(String, String),
}

fn unescape(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<String, String> {
    let mut s = String::new();
    loop {
        match chars.next().ok_or("unterminated string")? {
            '"' => return Ok(s),
            '\\' => match chars.next().ok_or("unterminated string")? {
                'n' => s.push('\n'),
                't' => s.push('\t'),
                'r' => s.push('\r'),
                c => s.push(c),
            },
            c => s.push(c),
        }
    }
}

fn tokenize(line: &str) -> Result<Vec<Tok>, String> {
    let mut toks = vec![];
    let mut it = line.chars().peekable();
    while let Some(&c) = it.peek() {
        if c.is_whitespace() {
            it.next();
        } else if c == '"' {
            it.next();
            toks.push(Tok::Str(unescape(&mut it)?));
        } else {
            let mut w = String::new();
            while let Some(&c) = it.peek() {
                if c.is_whitespace() || c == '(' || c == '"' {
                    break;
                }
                w.push(c);
                it.next();
            }
            if it.peek() != Some(&'(') {
                toks.push(Tok::Word(w));
                continue;
            }
            if w.is_empty() {
                return Err("`(` without a name".into());
            }
            it.next();
            let (mut depth, mut in_str, mut esc, mut args) = (1, false, false, String::new());
            loop {
                let c = it.next().ok_or_else(|| format!("unclosed `{w}(`"))?;
                if in_str {
                    in_str = esc || c != '"';
                    esc = !esc && c == '\\';
                } else if c == '"' {
                    in_str = true;
                } else if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                args.push(c);
            }
            toks.push(Tok::Call(w, args));
        }
    }
    Ok(toks)
}

/// Split on top-level commas (outside quotes and brackets).
fn split_top(raw: &str) -> Vec<&str> {
    let (mut parts, mut start, mut depth, mut in_str, mut esc) = (vec![], 0, 0, false, false);
    for (i, c) in raw.char_indices() {
        if in_str {
            in_str = esc || c != '"';
            esc = !esc && c == '\\';
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&raw[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&raw[start..]);
    parts
}

fn parse_scalar(s: &str) -> Result<Json, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty value".into());
    }
    if let Some(rest) = s.strip_prefix('"') {
        let mut it = rest.chars().peekable();
        let v = unescape(&mut it)?;
        return if it.next().is_none() {
            Ok(Json::String(v))
        } else {
            Err(format!("bad string {s}"))
        };
    }
    match s {
        "true" => return Ok(Json::Bool(true)),
        "false" => return Ok(Json::Bool(false)),
        "null" => return Ok(Json::Null),
        _ => {}
    }
    let numeric = s
        .trim_start_matches(['-', '+'])
        .starts_with(|c: char| c.is_ascii_digit() || c == '.');
    if numeric {
        if let Ok(i) = s.parse::<i64>() {
            return Ok(Json::from(i));
        }
        if let Some(n) = s.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
            return Ok(Json::Number(n));
        }
    }
    Ok(Json::String(s.to_string()))
}

fn parse_value(raw: &str) -> Result<Json, String> {
    let raw = raw.trim();
    if raw.starts_with('{') || raw.starts_with('[') {
        return serde_json::from_str(raw).map_err(|e| format!("bad JSON: {e}"));
    }
    let parts = split_top(raw);
    if parts.len() == 1 {
        return parse_scalar(parts[0]);
    }
    parts
        .into_iter()
        .map(parse_scalar)
        .collect::<Result<_, _>>()
        .map(Json::Array)
}

fn parse_nums(name: &str, raw: &str, n: Option<usize>) -> Result<Vec<f64>, String> {
    let v: Vec<f64> = split_top(raw)
        .into_iter()
        .map(|p| {
            p.trim()
                .parse::<f64>()
                .map_err(|_| format!("`{name}` expects numbers, got `{raw}`"))
        })
        .collect::<Result<_, _>>()?;
    match n {
        Some(n) if v.len() != n => Err(format!("`{name}` expects {n} numbers, got {}", v.len())),
        _ => Ok(v),
    }
}

fn parse_str(name: &str, raw: &str) -> Result<String, String> {
    match parse_value(raw)? {
        Json::String(s) => Ok(s),
        Json::Number(n) => Ok(n.to_string()),
        v => Err(format!("`{name}` expects a string, got {v}")),
    }
}

fn actor_ref(raw: &str) -> Result<Option<String>, String> {
    let s = parse_str("actor", raw)?;
    Ok(match s.as_str() {
        "null" | "-" | "" => None,
        _ => Some(s.trim_start_matches('@').to_string()),
    })
}

#[derive(Debug, Default)]
struct NodeRow {
    id: Option<String>,
    kind: Option<NodeKind>,
    props: Props,
    text: Option<String>,
    /// `Some((None, _))` = move to top level.
    parent: Option<(Option<String>, Option<usize>)>,
}

#[derive(Debug, Default)]
struct AnnRow {
    id: Option<String>,
    status: Option<Status>,
    anchor: Option<Anchor>,
    assignee: Option<Option<String>>,
    outcome: Option<Vec<String>>,
    on: Option<String>,
    text: Option<String>,
}

#[derive(Debug)]
enum RowKind {
    Node(NodeRow),
    Ann(AnnRow),
    Msg { echo: bool, text: String },
    Delete(Vec<String>),
}

#[derive(Debug)]
struct Row {
    line: usize,
    depth: usize,
    kind: RowKind,
}

fn region_of(name: &str, raw: &str) -> Result<Option<Region>, String> {
    Ok(Some(match name {
        "pt" => {
            let v = parse_nums(name, raw, Some(2))?;
            Region::Point { x: v[0], y: v[1] }
        }
        "box" => {
            let v = parse_nums(name, raw, Some(4))?;
            Region::Box {
                x: v[0],
                y: v[1],
                w: v[2],
                h: v[3],
            }
        }
        "poly" => {
            let v = parse_nums(name, raw, None)?;
            if v.len() % 2 != 0 {
                return Err("`poly` expects x,y pairs".into());
            }
            Region::Polygon {
                points: v.chunks(2).map(|c| [c[0], c[1]]).collect(),
            }
        }
        "mask" => Region::Mask {
            asset: parse_str(name, raw)?,
        },
        _ => return Ok(None),
    }))
}

fn parse_anchor(parts: &[(String, String)]) -> Result<Option<Anchor>, String> {
    let Some((name, raw)) = parts.first() else {
        return Ok(None);
    };
    let rest = &parts[1..];
    let region = || match rest {
        [] => Ok(None),
        [(n, r)] => region_of(n, r)?
            .map(Some)
            .ok_or_else(|| format!("`{n}` is not a region")),
        _ => Err("too many anchor parts".to_string()),
    };
    let one = |n| parse_nums(name, raw, Some(n));
    let a = match name.as_str() {
        "t" => Anchor::Time { t: one(1)?[0] },
        "range" => {
            let v = one(2)?;
            Anchor::Range { t0: v[0], t1: v[1] }
        }
        "frame" => Anchor::Frame {
            t: one(1)?[0],
            region: region()?,
        },
        "page" => Anchor::Page {
            n: one(1)?[0] as u32,
            region: region()?,
        },
        "chars" => {
            let v = one(2)?;
            Anchor::TextRange {
                start: v[0] as u32,
                end: v[1] as u32,
            }
        }
        "pt3" => {
            let p = one(3)?;
            let [(cn, craw)] = rest else {
                return Err("`pt3` needs `cam(px,py,pz,tx,ty,tz,fov)`".into());
            };
            if cn != "cam" {
                return Err("`pt3` needs `cam(...)`".into());
            }
            let c = parse_nums("cam", craw, Some(7))?;
            Anchor::Point3d {
                p: [p[0], p[1], p[2]],
                camera: Camera {
                    pos: [c[0], c[1], c[2]],
                    target: [c[3], c[4], c[5]],
                    fov: c[6],
                },
            }
        }
        _ => {
            if !rest.is_empty() {
                return Err(format!("`{name}` takes no region"));
            }
            match region_of(name, raw)?.ok_or_else(|| format!("unknown anchor `{name}`"))? {
                Region::Point { x, y } => Anchor::Point { x, y },
                Region::Box { x, y, w, h } => Anchor::Box { x, y, w, h },
                Region::Polygon { points } => Anchor::Polygon { points },
                Region::Mask { asset } => Anchor::Mask { asset },
            }
        }
    };
    Ok(Some(a))
}

const ANCHOR_CALLS: &[&str] = &[
    "pt", "box", "poly", "mask", "t", "range", "frame", "chars", "page", "pt3", "cam",
];

fn parse_ann(toks: &[Tok]) -> Result<AnnRow, String> {
    let mut r = AnnRow::default();
    let mut anchor = vec![];
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::Word(w) => match Status::parse(w) {
                Some(s) => r.status = Some(s),
                None if i == 0 => r.id = Some(w.clone()),
                None => {
                    return Err(format!(
                        "unexpected `{w}` (status is open|in_progress|resolved)"
                    ));
                }
            },
            Tok::Str(s) => r.text = Some(s.clone()),
            Tok::Call(n, a) if ANCHOR_CALLS.contains(&n.as_str()) => {
                anchor.push((n.clone(), a.clone()))
            }
            Tok::Call(n, a) => match n.as_str() {
                "to" => r.assignee = Some(actor_ref(a)?),
                "on" => r.on = Some(parse_str(n, a)?),
                "out" => {
                    r.outcome = Some(match parse_value(a)? {
                        Json::Array(v) => v
                            .iter()
                            .map(|x| value(x).trim_matches('"').to_string())
                            .collect(),
                        v => vec![value(&v).trim_matches('"').to_string()],
                    })
                }
                "by" => {}
                _ => return Err(format!("unknown annotation field `{n}`")),
            },
        }
    }
    r.anchor = parse_anchor(&anchor)?;
    Ok(r)
}

fn parse_node(toks: &[Tok]) -> Result<NodeRow, String> {
    let mut r = NodeRow::default();
    let mut it = toks.iter().peekable();
    let Some(Tok::Word(first)) = it.next() else {
        return Err("row must start with an id or a type".into());
    };
    match NodeKind::parse(first) {
        Some(k) => r.kind = Some(k),
        None => {
            r.id = Some(first.clone());
            if let Some(Tok::Word(w)) = it.peek()
                && let Some(k) = NodeKind::parse(w)
            {
                r.kind = Some(k);
                it.next();
            }
        }
    }
    if let Some(Tok::Str(name)) = it.peek() {
        r.props.insert("name".into(), Json::String(name.clone()));
        it.next();
    }
    let num = |v: f64| Json::from(serde_json::Number::from_f64(v).unwrap_or(0.into()));
    for t in it {
        match t {
            Tok::Word(w) if NodeKind::parse(w).is_some() => {
                return Err(format!("type must come right after the id, found `{w}`"));
            }
            Tok::Word(w) => {
                r.props.insert(w.clone(), Json::Bool(true));
            }
            Tok::Str(s) => return Err(format!("unexpected string {}", quote(s))),
            Tok::Call(n, a) => match n.as_str() {
                "p" | "s" => {
                    let v = parse_nums(n, a, Some(2))?;
                    let (kx, ky) = if n == "p" { ("x", "y") } else { ("w", "h") };
                    r.props.insert(kx.into(), num(v[0]));
                    r.props.insert(ky.into(), num(v[1]));
                }
                "r" => {
                    r.props
                        .insert("rot".into(), num(parse_nums(n, a, Some(1))?[0]));
                }
                "name" => {
                    r.props
                        .insert("name".into(), Json::String(parse_str(n, a)?));
                }
                "text" => r.text = Some(parse_str(n, a)?),
                "parent" => {
                    let parts = split_top(a);
                    let p = parse_str(n, parts[0])?;
                    let idx = match parts.get(1) {
                        Some(i) => Some(
                            i.trim()
                                .parse::<usize>()
                                .map_err(|_| "parent index must be >= 0")?,
                        ),
                        None => None,
                    };
                    r.parent = Some(((p != "root").then_some(p), idx));
                }
                "by" | "upd" | "more" => {}
                _ => {
                    r.props.insert(n.clone(), parse_value(a)?);
                }
            },
        }
    }
    Ok(r)
}

fn parse(text: &str) -> Result<Vec<Row>> {
    let mut rows = vec![];
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let body = raw.trim_start_matches([' ', '\t']);
        if body.trim().is_empty() || body.starts_with('#') {
            continue;
        }
        let indent: usize = raw[..raw.len() - body.len()]
            .chars()
            .map(|c| if c == '\t' { 2 } else { 1 })
            .sum();
        let err = |m: String| Error::Line {
            line,
            source: Box::new(Error::Invalid(m)),
        };
        let toks = tokenize(body).map_err(err)?;
        let kind = match toks.first() {
            Some(Tok::Word(w)) if w == "!" => RowKind::Ann(parse_ann(&toks[1..]).map_err(err)?),
            Some(Tok::Word(w)) if w == ">" => {
                let (echo, rest) = match toks.get(1) {
                    Some(Tok::Word(a)) if a.starts_with('@') => (true, &toks[2..]),
                    _ => (false, &toks[1..]),
                };
                match rest {
                    [Tok::Str(s)] => RowKind::Msg {
                        echo,
                        text: s.clone(),
                    },
                    _ => return Err(err("message row is `> \"text\"`".into())),
                }
            }
            Some(Tok::Call(n, a)) if n == "delete" => {
                if toks.len() > 1 {
                    return Err(err("delete(...) must be alone on its row".into()));
                }
                RowKind::Delete(
                    split_top(a)
                        .into_iter()
                        .map(|s| s.trim().to_string())
                        .collect(),
                )
            }
            _ => RowKind::Node(parse_node(&toks).map_err(err)?),
        };
        rows.push(Row {
            line,
            depth: indent / 2,
            kind,
        });
    }
    Ok(rows)
}

enum Target {
    Node(String),
    Ann { id: String, new: bool },
}

fn exec(tx: &mut Tx, rows: &[Row]) -> Result<()> {
    let mut stack: Vec<(usize, Target)> = vec![];
    for row in rows {
        while stack.last().is_some_and(|(d, _)| *d >= row.depth) {
            stack.pop();
        }
        let target = exec_row(tx, row, &stack).map_err(|e| Error::Line {
            line: row.line,
            source: Box::new(e),
        })?;
        if let Some(t) = target {
            stack.push((row.depth, t));
        }
    }
    Ok(())
}

fn exec_row(tx: &mut Tx, row: &Row, stack: &[(usize, Target)]) -> Result<Option<Target>> {
    let parent_node = stack.iter().rev().find_map(|(_, t)| match t {
        Target::Node(id) => Some(id.clone()),
        _ => None,
    });
    match &row.kind {
        RowKind::Node(r) => {
            let mut props = r.props.clone();
            let id = match &r.id {
                Some(id) if tx.has_node(id) => {
                    let kind = tx.node_kind(id)?;
                    if r.kind.is_some_and(|k| k != kind) {
                        return Err(Error::Invalid(format!(
                            "`{id}` is a {kind}; type can't change"
                        )));
                    }
                    if !props.is_empty() {
                        tx.update_node(id, &props)?;
                    }
                    if let Some(t) = &r.text {
                        tx.set_text(id, t)?;
                    }
                    if let Some((p, idx)) = &r.parent {
                        tx.move_node(id, p.as_deref(), *idx)?;
                    }
                    id.clone()
                }
                _ => {
                    let kind = r.kind.ok_or_else(|| match &r.id {
                        Some(id) => Error::UnknownNode(id.clone()),
                        None => Error::Invalid("missing type".into()),
                    })?;
                    let name = match props.remove("name") {
                        Some(Json::String(s)) => s,
                        _ => String::new(),
                    };
                    let (parent, index) = match &r.parent {
                        Some((p, i)) => (p.clone(), *i),
                        None => (parent_node, None),
                    };
                    tx.create_node(
                        r.id.as_deref(),
                        kind,
                        parent.as_deref(),
                        index,
                        &name,
                        &props,
                        r.text.as_deref(),
                    )?
                }
            };
            Ok(Some(Target::Node(id)))
        }
        RowKind::Ann(r) => {
            if let Some(id) = r.id.as_deref().filter(|id| tx.has_annotation(id)) {
                if r.status.is_some()
                    || r.anchor.is_some()
                    || r.assignee.is_some()
                    || r.outcome.is_some()
                {
                    let assignee = r.assignee.as_ref().map(|a| a.as_deref());
                    tx.update_annotation(
                        id,
                        r.status,
                        r.anchor.as_ref(),
                        assignee,
                        r.outcome.as_deref(),
                    )?;
                }
                if let Some(t) = &r.text {
                    tx.reply(id, t)?;
                }
                return Ok(Some(Target::Ann {
                    id: id.into(),
                    new: false,
                }));
            }
            let node = r.on.clone().or(parent_node).ok_or_else(|| {
                Error::Invalid(
                    "annotation row must be under a node row or have on(<node id>)".into(),
                )
            })?;
            let anchor = r.anchor.as_ref().ok_or_else(|| {
                Error::Invalid(
                    "new annotation needs an anchor: pt box poly mask t range frame chars page pt3"
                        .into(),
                )
            })?;
            let assignee = r.assignee.clone().flatten();
            let status = r.status.unwrap_or(Status::Open);
            let id = tx.add_annotation(
                r.id.as_deref(),
                &node,
                anchor,
                status,
                r.text.as_deref(),
                assignee.as_deref(),
            )?;
            if let Some(out) = &r.outcome {
                tx.update_annotation(&id, None, None, None, Some(out))?;
            }
            Ok(Some(Target::Ann { id, new: true }))
        }
        RowKind::Msg { echo, text } => {
            let Some((_, Target::Ann { id, new })) = stack.last() else {
                return Err(Error::Invalid(
                    "message row must be indented under an annotation row".into(),
                ));
            };
            // Echoed messages (`> @author "…"`) of an existing thread are ignored.
            if *new || !echo {
                tx.reply(id, text)?;
            }
            Ok(None)
        }
        RowKind::Delete(ids) => {
            for id in ids {
                if tx.has_node(id) {
                    tx.delete_node(id)?;
                } else if tx.has_annotation(id) {
                    tx.delete_annotation(id)?;
                } else {
                    return Err(Error::UnknownNode(id.clone()));
                }
            }
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_calls_strings_and_json() {
        let t = tokenize(r#"abc image "a \"b\"" p(1,2) params({"x":"(y)"}) loop"#).unwrap();
        assert_eq!(
            t,
            vec![
                Tok::Word("abc".into()),
                Tok::Word("image".into()),
                Tok::Str("a \"b\"".into()),
                Tok::Call("p".into(), "1,2".into()),
                Tok::Call("params".into(), r#"{"x":"(y)"}"#.into()),
                Tok::Word("loop".into()),
            ]
        );
    }

    #[test]
    fn values_round_trip() {
        for v in [
            serde_json::json!(12),
            serde_json::json!(1.5),
            serde_json::json!("sha:9f"),
            serde_json::json!("has space"),
            serde_json::json!("123"),
            serde_json::json!("true"),
            serde_json::json!([1, 2, 3]),
            serde_json::json!(["a"]),
            serde_json::json!(["a,b", "c"]),
            serde_json::json!({"prompt": "red car", "n": 2}),
        ] {
            assert_eq!(parse_value(&value(&v)).unwrap(), v, "{}", value(&v));
        }
    }
}
