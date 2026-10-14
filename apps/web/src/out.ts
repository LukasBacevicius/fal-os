// "Out": canvas assets into other apps (docs/ASSETS-V1.md §4, §5, §7). Drag-out, copy file /
// image / fal URL / prompt, Open in…, Reveal, export (host `POST /api/export`), ops
// (`/api/transform`, `/api/rerun`), roles, lineage and the Deliverables frame.
//
// Desktop: assets are materialized ahead of time (selection change → `materialize` → a
// human-named APFS clone in `<project>/.out/`) so a drag can start synchronously while the
// mouse button is still down. Web: downloads, `DownloadURL` drags and the async clipboard.
// Host endpoints that aren't there yet fail softly with a toast.

import { withProgress } from './progress';
import { getState, setState, toast } from './store';
import { hostSync } from './sync';
import { desktop, isDesktop, type AppInfo } from './desktop';
import type { NodeView } from './types';

// ---- nodes ---------------------------------------------------------------------------------

export const assetOf = (n: NodeView | undefined | null): string | null => {
  const a = n?.props?.asset;
  return typeof a === 'string' && a.startsWith('sha256:') ? a : null;
};

/** File name stem for a node: its name, else `<kind>-<id>`. */
export const nodeName = (n: NodeView) => (n.name || '').trim() || `${n.kind}-${n.id}`;

export const roleOf = (n: NodeView): string => String(n.props.role ?? '');

function byIds(ids: string[]): NodeView[] {
  const m = new Map(getState().nodes.map((n) => [n.id, n]));
  return ids.map((id) => m.get(id)).filter((n): n is NodeView => !!n);
}

/** The nodes among `ids` that have a stored asset (what can go out as a file). */
export const outNodes = (ids: string[] = getState().selection) => byIds(ids).filter((n) => assetOf(n));

/** The node's asset record (lineage, uri, fal…) from the doc. */
export function assetRecord(id: string): Record<string, any> | null {
  const d = getState().app?.node(id) as { asset?: Record<string, any> } | null;
  return d?.asset ?? null;
}

export type Source = 'import' | 'generate' | 'derive' | 'render';

/** How the bytes came to exist (mirrors `badge_of` in crates/client/src/overlay.rs): the
 *  Assets-v1 `source`, else the lineage kind, else the old generation lineage. */
export function sourceOf(rec: Record<string, any> | null): Source | null {
  if (!rec) return null;
  const s = typeof rec.source === 'string' ? rec.source : rec.source?.kind ?? rec.lineage?.kind;
  if (s === 'generate' || s === 'derive' || s === 'render') return s;
  if (s) return 'import';
  const m = rec.lineage?.model;
  return !m || m === 'import' ? 'import' : 'generate';
}

/** Layers filter chips (docs/ASSETS-V1.md §5). */
export const FILTERS = ['inputs', 'generated', 'derived', 'finals', 'rejected'] as const;
export type Filter = (typeof FILTERS)[number];

export function matchesFilter(n: NodeView, f: Filter, src: Source | null): boolean {
  const role = roleOf(n);
  switch (f) {
    case 'inputs': return role === 'input' || (!role && src === 'import');
    case 'generated': return src === 'generate';
    case 'derived': return src === 'derive' || src === 'render';
    case 'finals': return role === 'final';
    case 'rejected': return role === 'rejected';
  }
}

// ---- host REST -----------------------------------------------------------------------------

/** The host doesn't serve this endpoint (an older host or a work stream not landed yet). */
export class ApiMissing extends Error {}

export async function api<T = any>(path: string, body?: unknown, method = body === undefined ? 'GET' : 'POST'): Promise<T> {
  const sync = hostSync();
  if (!sync) throw new Error('no host connected');
  const q = sync.project ? `${path.includes('?') ? '&' : '?'}project=${encodeURIComponent(sync.project)}` : '';
  const r = await fetch(`http://${sync.info.addr}${path}${q}`, {
    method,
    headers: { Authorization: `Bearer ${sync.info.token}`, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await r.text();
  let v: any = null;
  try {
    v = text ? JSON.parse(text) : null;
  } catch {
    /* plain text */
  }
  // Axum's unknown-route 404/405 has no JSON error body.
  if ((r.status === 404 || r.status === 405) && !v?.error) throw new ApiMissing(path);
  if (!r.ok) throw new Error(String(v?.error ?? v?.message ?? text ?? r.status).slice(0, 300));
  return v as T;
}

/** Runs a host call; a missing endpoint or failure becomes a toast (returns null). */
async function soft<T>(label: string, f: () => Promise<T>): Promise<T | null> {
  try {
    return await f();
  } catch (e) {
    toast(e instanceof ApiMissing ? `${label}: not supported by this host yet` : `${label} failed: ${e instanceof Error ? e.message : e}`);
    return null;
  }
}

// ---- materialize (desktop) -----------------------------------------------------------------

const ready = new Map<string, string>(); // `${asset}|${name}` → path
const pending = new Map<string, Promise<string | null>>();
const key = (n: NodeView) => `${assetOf(n)}|${nodeName(n)}`;

async function materialize(n: NodeView): Promise<string | null> {
  const k = key(n);
  const have = ready.get(k);
  if (have) return have;
  const p = pending.get(k);
  if (p) return p;
  const project = hostSync()?.project ?? null;
  const run = (async () => {
    try {
      let path: string;
      if (isDesktop()) {
        try {
          path = await desktop.materialize({ sha: assetOf(n)!, name: nodeName(n), project });
        } catch {
          // Not in a local store the shell can see: the host resolves the node.
          path = await desktop.materialize({ node: n.id, name: nodeName(n), project });
        }
      } else {
        path = (await api<{ path: string }>('/api/materialize', { node: n.id, sha: assetOf(n), name: nodeName(n) })).path;
      }
      ready.set(k, path);
      return path;
    } catch (e) {
      console.warn('materialize failed', n.id, e);
      return null;
    } finally {
      pending.delete(k);
    }
  })();
  pending.set(k, run);
  return run;
}

let prepTimer = 0;
/** Selection changed: materialize its files now, so a drag-out can start synchronously. */
export function prepare(ids: string[], delay = 80) {
  if (!isDesktop()) return;
  window.clearTimeout(prepTimer);
  const run = () => outNodes(ids).slice(0, 50).forEach((n) => void materialize(n));
  if (delay) prepTimer = window.setTimeout(run, delay);
  else run();
}

/** Paths of already materialized files for `ids` (null if any is missing). */
export function readyPaths(ids: string[] = getState().selection): string[] | null {
  const ns = outNodes(ids);
  if (!ns.length) return null;
  const ps = ns.map((n) => ready.get(key(n)));
  return ps.every(Boolean) ? (ps as string[]) : null;
}

export async function pathsOf(ids: string[] = getState().selection): Promise<string[]> {
  const ps = await Promise.all(outNodes(ids).map(materialize));
  return ps.filter((p): p is string => !!p);
}

// ---- drag-out ------------------------------------------------------------------------------

/** Starts a native drag of the selection's files. Synchronous on purpose: call it from a
 *  pointer event while the button is down. Returns false when the files aren't ready yet. */
export function dragOut(ids: string[] = getState().selection): boolean {
  if (!isDesktop()) return false;
  const ps = readyPaths(ids);
  if (!ps) {
    if (outNodes(ids).length) {
      toast('preparing files… drag again');
      void pathsOf(ids);
    }
    return false;
  }
  desktop.dragOut(ps).catch((e) => toast(`drag failed: ${e}`));
  return true;
}

/** Chrome's file drag (web build): `DownloadURL` on a native dragstart. */
export function setDownloadUrl(e: React.DragEvent | DragEvent, n: NodeView): boolean {
  const sync = hostSync();
  const a = assetOf(n);
  const dt = e.dataTransfer;
  if (!sync || !a || !dt) return false;
  const url = sync.assetHref(a);
  const mime = (getState().app?.assets() as { id: string; mime?: string }[] | undefined)?.find((x) => x.id === a)?.mime || 'application/octet-stream';
  dt.setData('DownloadURL', `${mime}:${nodeName(n)}.${extOf(mime)}:${url}`);
  dt.setData('text/uri-list', url);
  dt.effectAllowed = 'copy';
  return true;
}

const EXT: Record<string, string> = {
  'image/png': 'png', 'image/jpeg': 'jpg', 'image/webp': 'webp', 'image/gif': 'gif', 'image/avif': 'avif', 'image/svg+xml': 'svg',
  'video/mp4': 'mp4', 'video/quicktime': 'mov', 'video/webm': 'webm', 'audio/mpeg': 'mp3', 'audio/wav': 'wav', 'application/pdf': 'pdf',
};
const extOf = (mime: string) => EXT[mime.split(';')[0]] ?? 'bin';

/** Web: downloads the nodes' files (blob URLs: `<a download>` ignores cross-origin hrefs). */
export async function download(ids: string[] = getState().selection) {
  const sync = hostSync();
  if (!sync) return toast('no host connected');
  for (const n of outNodes(ids)) {
    try {
      const blob = await sync.assetBlob(assetOf(n)!);
      const a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = `${nodeName(n)}.${extOf(blob.type)}`;
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 10_000);
    } catch (e) {
      toast(`download failed: ${e}`);
    }
  }
}

// ---- copy ----------------------------------------------------------------------------------

const clip = (text: string, what: string) =>
  navigator.clipboard?.writeText(text).then(() => toast(`${what} copied`), () => toast('copy failed')) ?? toast('copy failed');

/** Copy file(s): file URLs on the pasteboard (⌘V in Finder, Slack, Premiere). Web: URLs. */
export async function copyFiles(ids: string[] = getState().selection) {
  const ns = outNodes(ids);
  if (!ns.length) return toast('nothing to copy: no media selected');
  if (!isDesktop()) {
    const sync = hostSync();
    return clip(ns.map((n) => sync?.assetHref(assetOf(n)!) ?? '').join('\n'), ns.length > 1 ? 'links' : 'link');
  }
  const ps = await pathsOf(ids);
  if (!ps.length) return toast('copy failed: files not available');
  await desktop.copyFiles(ps).then(() => toast(ps.length > 1 ? `${ps.length} files copied` : 'file copied'), (e) => toast(`copy failed: ${e}`));
}

/** Copy image: PNG on the clipboard. */
export async function copyImage(id: string = getState().selection[0]) {
  const n = outNodes([id])[0];
  if (!n || n.kind !== 'image') return toast('copy image: select an image');
  if (isDesktop()) {
    const [p] = await pathsOf([id]);
    if (!p) return toast('copy failed: file not available');
    return desktop.copyImage(p).then(() => toast('image copied'), (e) => toast(`copy failed: ${e}`));
  }
  try {
    const blob = await hostSync()!.assetBlob(assetOf(n)!);
    const png = blob.type === 'image/png' ? blob : await toPng(blob);
    await navigator.clipboard.write([new ClipboardItem({ 'image/png': png })]);
    toast('image copied');
  } catch (e) {
    toast(`copy failed: ${e}`);
  }
}

async function toPng(blob: Blob): Promise<Blob> {
  const bmp = await createImageBitmap(blob);
  const c = document.createElement('canvas');
  c.width = bmp.width;
  c.height = bmp.height;
  c.getContext('2d')!.drawImage(bmp, 0, 0);
  return await new Promise((res, rej) => c.toBlob((b) => (b ? res(b) : rej(new Error('encode'))), 'image/png'));
}

/** The asset's fal CDN URL (generated media, or an import uploaded to fal). */
export function falUrl(id: string): string | null {
  const a = assetRecord(id);
  const u = a?.fal?.url ?? a?.uri;
  return typeof u === 'string' && /^https?:\/\//.test(u) ? u : null;
}

export function copyFalUrl(id: string = getState().selection[0]) {
  const u = falUrl(id);
  if (!u) return toast('not on fal (local only)');
  clip(u, 'fal URL');
}

/** Prompt of a generated node (lineage params / recipe, or a generation's params). */
export function promptOf(id: string): string {
  const n = byIds([id])[0];
  const lin = assetRecord(id)?.lineage;
  const p = lin?.recipe?.prompt ?? lin?.params?.prompt ?? (n?.props.params as any)?.prompt ?? '';
  return typeof p === 'string' ? p : '';
}

export function copyPrompt(id: string = getState().selection[0]) {
  const p = promptOf(id);
  if (!p) return toast('no prompt on this node');
  clip(p, 'prompt');
}

export function copyRecipe(id: string = getState().selection[0]) {
  const lin = assetRecord(id)?.lineage;
  if (!lin) return toast('no recipe (imported)');
  const recipe = { op: lin.op ?? lin.model, recipe: lin.recipe ?? lin.params, seed: lin.seed ?? undefined };
  clip(JSON.stringify(recipe, null, 2), 'recipe');
}

// ---- reveal / open in -----------------------------------------------------------------------

export async function reveal(target: string | string[] = getState().selection) {
  if (!isDesktop()) return Array.isArray(target) ? download(target) : toast(target);
  const path = Array.isArray(target) ? (await pathsOf(target))[0] : target;
  if (!path) return toast('nothing to reveal');
  desktop.reveal(path).catch((e) => toast(`reveal failed: ${e}`));
}

/** Apps for "Open in…" (desktop): the file path plus the list. */
export async function openWithList(target: string | string[]): Promise<{ path: string; apps: AppInfo[] } | null> {
  if (!isDesktop()) return null;
  const path = Array.isArray(target) ? (await pathsOf(target))[0] : target;
  if (!path) return null;
  try {
    return { path, apps: await desktop.openWithList(path) };
  } catch (e) {
    toast(`open in: ${e}`);
    return null;
  }
}

export function openWith(path: string, app: AppInfo) {
  desktop.openWith(path, app).catch((e) => toast(`open failed: ${e}`));
}

// ---- roles ---------------------------------------------------------------------------------

/** Sets `role` on the nodes (`null` removes it). */
export function setRole(ids: string[], role: string | null) {
  const { app } = getState();
  if (!app || !ids.length) return;
  try {
    app.apply(ids.map((id) => ({ op: 'update', id, props: { role } })));
    toast(role ? `marked ${role}` : 'role cleared');
  } catch (e) {
    toast(String(e));
  }
}

export function toggleFinal(ids: string[] = getState().selection) {
  const ns = byIds(ids);
  if (!ns.length) return;
  setRole(ids, ns.every((n) => roleOf(n) === 'final') ? null : 'final');
}

/** A frame named "Deliverables" (or with `deliverables: true`). */
export const isDeliverables = (n: NodeView | undefined) =>
  !!n && n.kind === 'frame' && (n.props.deliverables === true || n.name.trim().toLowerCase() === 'deliverables');

let parents: Map<string, string | null> | null = null;
/** Dropping a node into the Deliverables frame marks it final (only the local user's moves). */
export function watchDeliverables(nodes: NodeView[]) {
  const { app } = getState();
  const prev = parents;
  parents = new Map(nodes.map((n) => [n.id, n.parent]));
  if (!prev || !app) return;
  const me = (app.actor() as { id?: string } | null)?.id;
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const marks = nodes.filter((n) => {
    if (!n.parent || prev.get(n.id) === n.parent || !prev.has(n.id)) return false;
    return isDeliverables(byId.get(n.parent)) && roleOf(n) !== 'final' && (!me || n.updatedBy === me);
  });
  if (!marks.length) return;
  try {
    app.apply(marks.map((n) => ({ op: 'update', id: n.id, props: { role: 'final' } })));
    toast(marks.length > 1 ? `${marks.length} nodes marked final` : 'marked final (deliverables)');
  } catch {
    /* read-only */
  }
}

// ---- lineage --------------------------------------------------------------------------------

export interface ParentRef { asset: string; node?: string; role?: string }

/** Lineage parents in either shape: old `parents: [sha]`, new `[{asset, node?, role}]`. */
export function parentRefs(lineage: any): ParentRef[] {
  const ps = Array.isArray(lineage?.parents) ? lineage.parents : [];
  return ps
    .map((p: any) => (typeof p === 'string' ? { asset: p, role: 'input' } : p && typeof p.asset === 'string' ? { asset: p.asset, node: typeof p.node === 'string' ? p.node : undefined, role: p.role } : null))
    .filter(Boolean) as ParentRef[];
}

/** Node graph from lineage: node → parent nodes (by `node` ref, else nodes showing the asset). */
export function lineageGraph(nodes: NodeView[], recordOf: (id: string) => Record<string, any> | null) {
  const byAsset = new Map<string, string[]>();
  for (const n of nodes) {
    const a = assetOf(n);
    if (a) byAsset.set(a, [...(byAsset.get(a) ?? []), n.id]);
  }
  const ids = new Set(nodes.map((n) => n.id));
  const up = new Map<string, string[]>();
  const down = new Map<string, string[]>();
  for (const n of nodes) {
    if (!assetOf(n)) continue;
    const refs = parentRefs(recordOf(n.id)?.lineage);
    const ps = new Set<string>();
    for (const r of refs) {
      if (r.node && ids.has(r.node)) ps.add(r.node);
      else for (const id of byAsset.get(r.asset) ?? []) if (id !== n.id) ps.add(id);
    }
    up.set(n.id, [...ps]);
    for (const p of ps) down.set(p, [...(down.get(p) ?? []), n.id]);
  }
  return { up, down };
}

/** The ids plus all their ancestors and descendants. */
export function lineageOf(ids: string[], g: ReturnType<typeof lineageGraph>): Set<string> {
  const keep = new Set(ids);
  for (const dir of [g.up, g.down]) {
    const stack = [...ids];
    while (stack.length) {
      const id = stack.pop()!;
      for (const m of dir.get(id) ?? []) if (!keep.has(m)) keep.add(m), stack.push(m);
    }
  }
  return keep;
}

/** Applies the lineage view for the current selection (or clears it). */
export function syncLineage() {
  const { app, nodes, selection, lineage } = getState();
  if (!app) return;
  if (!lineage || !selection.length) {
    app.setLineage(null);
    return;
  }
  const g = lineageGraph(nodes, assetRecord);
  app.setLineage([...lineageOf(selection, g)]);
}

export function toggleLineage() {
  const on = !getState().lineage && getState().selection.length > 0;
  setState({ lineage: on });
  syncLineage();
  if (on) toast('lineage · esc to leave');
}

/** Parents whose node now shows different bytes than the lineage recorded. */
export function upstreamChanged(id: string): string[] {
  const nodes = new Map(getState().nodes.map((n) => [n.id, n]));
  return parentRefs(assetRecord(id)?.lineage)
    .filter((r) => r.node && nodes.has(r.node) && assetOf(nodes.get(r.node)) !== r.asset)
    .map((r) => r.node!);
}

// ---- ops (host op registry) -----------------------------------------------------------------

/** `POST /api/transform` on the selection: placeholders appear, progress comes as jobs. */
export async function transform(op: string, args: Record<string, unknown> = {}, ids: string[] = getState().selection) {
  if (!ids.length) return toast(`${op}: select something first`);
  const r = await soft(op, () => api<{ nodes?: string[]; placeholders?: string[] }>('/api/transform', { op, inputs: ids, args }));
  if (r) toast(`${op.replace(/_/g, ' ')} · ${(r.nodes ?? r.placeholders ?? []).length || ids.length} running`);
}

export async function rerun(id: string = getState().selection[0], overrides?: Record<string, unknown>, on?: string[]) {
  if (!id) return;
  const r = await soft('rerun', () => api('/api/rerun', { node: id, overrides, on }));
  if (r) toast(on?.length ? `applying to ${on.length}` : 'rerunning');
}

/** "Apply to selection": the first selected node's recipe on the others. */
export function applyRecipe() {
  const [src, ...rest] = getState().selection;
  if (!src || !rest.length) return toast('select the source first, then the targets');
  void rerun(src, undefined, rest);
}

// ---- export ---------------------------------------------------------------------------------

/** One export row: a preset, or a format at a scale (`2x`, `1080p`, `512w`), + suffix. */
export interface ExportRow { preset?: string; format?: string; scale?: string; suffix?: string }

export interface Preset { id: string; label: string; kind?: string }

/** Built-in list (spec §4) for hosts without `GET /api/presets`. */
export const FALLBACK_PRESETS: Preset[] = [
  'reel', 'tiktok', 'short', 'ig-feed-45', 'square', 'youtube-1080', 'youtube-4k', 'x', 'linkedin',
  'master-prores', 'overlay-alpha', 'web-image', 'gif-web', 'wav', 'mp3',
].map((id) => ({ id, label: id }));

let presets: Preset[] | null = null;
export async function loadPresets(): Promise<Preset[]> {
  if (presets) return presets;
  try {
    const v = await api<any>('/api/presets');
    const list = Array.isArray(v) ? v : Array.isArray(v?.presets) ? v.presets : v && typeof v === 'object' ? Object.entries(v.presets ?? v).map(([id, p]) => ({ id, ...(p as object) })) : [];
    const out = list
      .map((p: any) => (typeof p === 'string' ? { id: p, label: p } : { id: String(p.id ?? p.name ?? ''), label: String(p.label ?? p.name ?? p.id ?? ''), kind: p.kind ?? p.media }))
      .filter((p: Preset) => p.id);
    presets = out.length ? out : FALLBACK_PRESETS;
  } catch {
    return FALLBACK_PRESETS; // no host yet, or no presets endpoint: ask again next time
  }
  return presets!;
}

/** Paper's scale grammar: `0.5x`, `2x`, `720p`, `512w`, `512h`. */
export const validScale = (s: string) => !s || /^(\d+(\.\d+)?x|\d+[pwh])$/.test(s.trim());

export interface ExportResult { files: { path: string; name?: string; node?: string; preset?: string; w?: number; h?: number; bytes?: number; sha256?: string }[]; dir: string }

const payloadRows = (rows: ExportRow[]) =>
  rows.map((r) => (r.preset && !r.format && !r.scale && !r.suffix ? r.preset : Object.fromEntries(Object.entries(r).filter(([, v]) => v))));

/** `POST /api/export`; toasts the result with [reveal] [copy]. */
export async function runExport(
  target: string[] | 'deliverables',
  rows: ExportRow[],
  opts: { dir?: string; naming?: string; combine?: 'zip' | 'pdf' } = {},
): Promise<ExportResult | null> {
  if (!rows.length) {
    toast('export: add a preset or format');
    return null;
  }
  const body: Record<string, unknown> = { nodes: target, presets: payloadRows(rows), ...opts };
  // `exported …` (announce) is the done line; the progress line only bridges a slow export.
  const r = await withProgress('exporting', () => soft('export', () => api<ExportResult>('/api/export', body)), { key: 'export' });
  if (!r) return null;
  announce(r);
  return r;
}

export function announce(r: ExportResult) {
  const paths = (r.files ?? []).map((f) => f.path);
  const actions = isDesktop() && paths.length
    ? [
        { label: 'reveal', run: () => reveal(paths[0]) },
        { label: 'copy', run: () => desktop.copyFiles(paths).then(() => toast('files copied'), (e) => toast(`copy failed: ${e}`)) },
      ]
    : [];
  toast(`exported ${paths.length} file${paths.length === 1 ? '' : 's'}${r.dir ? ` → ${r.dir.split('/').slice(-2).join('/')}` : ''}`, actions);
}

/** Saved per-node export settings (`export` prop). */
export const exportRowsOf = (n: NodeView): ExportRow[] => (Array.isArray(n.props.export) ? (n.props.export as ExportRow[]) : []);

/** ⌘⇧E with nothing selected: every node with saved export settings, each with its rows. */
export async function exportMarked() {
  const marked = getState().nodes.filter((n) => exportRowsOf(n).length);
  if (!marked.length) return toast('no nodes with export settings · select nodes, ⌘⇧E');
  const groups = new Map<string, string[]>();
  for (const n of marked) {
    const k = JSON.stringify(exportRowsOf(n));
    groups.set(k, [...(groups.get(k) ?? []), n.id]);
  }
  const files: ExportResult['files'] = [];
  let dir = '';
  for (const [k, ids] of groups) {
    const r = await soft('export', () => api<ExportResult>('/api/export', { nodes: ids, presets: payloadRows(JSON.parse(k)) }));
    if (!r) return;
    files.push(...(r.files ?? []));
    dir = r.dir || dir;
  }
  announce({ files, dir });
}

/** Opens the export panel for the selection (nothing selected: export all marked). */
export function openExport() {
  const sel = getState().selection;
  setState({ overlay: 'export', exportIds: sel, menu: null });
}
