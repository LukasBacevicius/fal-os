//! Implicit presence for MCP agents. Agents rarely send cursors, so whenever a session reads
//! or changes nodes its cursor moves to the center of the last node it touched (and its
//! viewport hint to that node plus a margin). Humans then see agents "work" on the canvas.

use falos_doc::Canvas;

use crate::host::Host;

/// World rect `[x, y, w, h]` of a node. Positions are parent-relative (FORMAT.md), so the
/// ancestors' positions are added up.
pub fn world_rect(c: &Canvas, id: &str) -> Option<[f64; 4]> {
    let n = c.node(id)?;
    let (mut x, mut y) = (n.x, n.y);
    let mut parent = n.parent.clone();
    let mut guard = 0;
    while let Some(p) = parent.take()
        && guard < 64
    {
        guard += 1;
        let Some(pn) = c.node(&p) else { break };
        x += pn.x;
        y += pn.y;
        parent = pn.parent;
    }
    Some([x, y, n.w, n.h])
}

impl Host {
    /// Point `session`'s presence at the last of `ids` that exists on `canvas`.
    /// A no-op for unknown sessions/nodes; never fails the calling tool.
    pub fn touch_nodes(&self, session: &str, canvas: &str, ids: &[String]) {
        let Ok(canvas) = self.resolve_canvas(canvas) else {
            return;
        };
        let rect = self
            .read(&canvas, |c| {
                ids.iter().rev().find_map(|id| world_rect(c, id))
            })
            .ok()
            .flatten();
        self.touch_rect(session, &canvas, rect);
    }

    /// Like [`Host::touch_nodes`] for an explicit point inside a node (e.g. an annotation pin).
    pub fn touch_point(&self, session: &str, canvas: &str, node: &str, local: [f64; 2]) {
        let Ok(canvas) = self.resolve_canvas(canvas) else {
            return;
        };
        let rect = self.read(&canvas, |c| world_rect(c, node)).ok().flatten();
        if let Some(r) = rect {
            let p = [r[0] + local[0] * r[2], r[1] + local[1] * r[3]];
            let _ = self.set_presence(session, |pr| {
                pr.canvas = Some(canvas.clone());
                pr.cursor = Some(p);
                pr.viewport = Some(margin(r));
            });
        }
    }

    fn touch_rect(&self, session: &str, canvas: &str, rect: Option<[f64; 4]>) {
        let Some(r) = rect else { return };
        let _ = self.set_presence(session, |p| {
            p.canvas = Some(canvas.to_string());
            p.cursor = Some([r[0] + r[2] * 0.5, r[1] + r[3] * 0.5]);
            p.viewport = Some(margin(r));
        });
    }
}

fn margin(r: [f64; 4]) -> [f64; 4] {
    let (mx, my) = (r[2] * 0.25, r[3] * 0.25);
    [r[0] - mx, r[1] - my, r[2] + 2.0 * mx, r[3] + 2.0 * my]
}
