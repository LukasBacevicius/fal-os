// Previews: what kind of viewer a file gets, and the canvas preview (quick look of a node:
// space tap, ⏎ or double-click). The viewers themselves are in components/Preview.tsx and
// are shared with the library's quick look. Videos keep their own focus view (video.ts).

import { getState, setState, type AssetInfo } from './store';
import { worldRects } from './geometry';
import type { NodeView } from './types';
import { assetRecord } from './out';

export type PreviewKind =
  | 'image' | 'svg' | 'video' | 'audio' | 'pdf' | 'model3d' | 'usdz' | 'font' | 'lut' | 'subs'
  | 'markdown' | 'text' | 'html' | 'json' | 'timeline' | 'other';

/** Everything a viewer needs; built from a canvas node or a library item. */
export interface PreviewItem {
  /** Stable id (node id or `lib:<sha>`): the audio player key. */
  key: string;
  kind: PreviewKind;
  name: string;
  mime: string;
  /** The original (tokened host URL or blob: URL); '' when there is none. */
  url: string;
  /** A large still to fall back on (library 1024 thumbnail) or show as a poster. */
  poster?: string;
  /** sha256 hex (host-side renders: PDF pages). */
  sha?: string;
  bytes?: number | null;
  w?: number | null;
  h?: number | null;
  duration?: number | null;
  /** Text already at hand (a note's body), instead of fetching `url`. */
  text?: string | null;
  /** The canvas node (reveal / open go through the node's asset). */
  node?: string;
  /** An image to try a LUT on. */
  sample?: string;
}

/** Another host path on the same host as a tokened asset URL (keeps only the token), or
 *  null for local blob: URLs. */
export function hostPath(assetUrl: string, path: string): string | null {
  try {
    const u = new URL(assetUrl);
    if (u.protocol !== 'http:' && u.protocol !== 'https:') return null;
    const token = u.searchParams.get('token');
    const out = new URL(path, u.origin);
    if (token) out.searchParams.set('token', token);
    return out.toString();
  } catch {
    return null;
  }
}

const ext = (name: string) => (name.match(/\.([a-z0-9]+)$/i)?.[1] ?? '').toLowerCase();

/** The viewer for a mime type / file name (and canvas node kind when known). */
export function previewKind(mime: string, name: string, nodeKind?: string): PreviewKind {
  const m = (mime || '').split(';')[0].trim().toLowerCase();
  const e = ext(name);
  if (m === 'image/svg+xml' || e === 'svg' || nodeKind === 'vector') return 'svg';
  if (e === 'usdz' || m === 'model/vnd.usdz+zip') return 'usdz';
  if (nodeKind === 'model3d' || m.startsWith('model/') || ['glb', 'gltf'].includes(e)) return 'model3d';
  if (m === 'application/pdf' || e === 'pdf') return 'pdf';
  if (m.startsWith('font/') || ['ttf', 'otf', 'woff', 'woff2'].includes(e) || nodeKind === 'style:font') return 'font';
  if (e === 'cube' || m === 'application/x-cube-lut' || nodeKind === 'style:lut') return 'lut';
  if (['srt', 'vtt'].includes(e) || m === 'application/x-subrip' || m === 'text/vtt') return 'subs';
  if (e === 'otio' || m === 'application/vnd.otio+json' || nodeKind === 'timeline') return 'timeline';
  if (['html', 'htm'].includes(e) || m === 'text/html') return 'html';
  if (['md', 'markdown'].includes(e) || m === 'text/markdown') return 'markdown';
  if (e === 'json' || m === 'application/json') return 'json';
  if (nodeKind === 'video' || m.startsWith('video/') || ['mp4', 'mov', 'webm', 'm4v', 'mkv'].includes(e)) return 'video';
  if (nodeKind === 'audio' || m.startsWith('audio/') || ['mp3', 'wav', 'm4a', 'aac', 'ogg', 'flac', 'aiff', 'aif'].includes(e)) return 'audio';
  if (nodeKind === 'image' || m.startsWith('image/')) return 'image';
  if (nodeKind === 'text' || nodeKind === 'note' || m.startsWith('text/') || ['txt', 'csv', 'log'].includes(e)) return 'text';
  return 'other';
}

/** Short type label: `[pdf]`, `[3d]`, … */
export function kindLabel(k: PreviewKind, mime = '', name = ''): string {
  const e = ext(name);
  switch (k) {
    case 'model3d': case 'usdz': return '3d';
    case 'markdown': return 'md';
    case 'subs': return e || 'subs';
    case 'other': return e || mime.split('/')[1] || 'file';
    default: return k;
  }
}

const NOT_MEDIA = new Set(['frame', 'group', 'connector', 'generation', 'stream', 'embed']);

/** Nodes the canvas preview can show (videos have their own focus view). */
export function previewable(n: NodeView): boolean {
  if (NOT_MEDIA.has(n.kind) || n.kind === 'video') return false;
  if (n.kind === 'note') return false; // notes are edited in place
  // Imported documents (.md / .txt carry a role), not canvas text such as Figma layers.
  if (n.kind === 'text') return !!(n.props.asset || (n.props.role && n.text));
  return typeof n.props.asset === 'string';
}

/** The preview item of a canvas node. */
export function nodeItem(n: NodeView): PreviewItem {
  const app = getState().app;
  const asset = typeof n.props.asset === 'string' ? n.props.asset : null;
  const rec = asset ? assetRecord(n.id) : null;
  const info = asset && app ? (app.assets() as AssetInfo[]).find((a) => a.id === asset) : undefined;
  const mime = String(rec?.mime ?? info?.mime ?? '');
  const name = String(rec?.origin?.filename ?? info?.name ?? n.name ?? '') || n.name;
  const media = rec?.media ?? {};
  let kind = previewKind(mime, name, n.kind);
  // Imported .md / .txt become text nodes holding the text: markdown if it reads like it.
  if (kind === 'text' && n.text && /^(#{1,6} |[-*+] |\d+[.)] |```|> )/m.test(n.text)) kind = 'markdown';
  return {
    key: n.id,
    kind,
    name: n.name || name,
    mime,
    url: asset ? app?.assetUrl(asset) ?? '' : '',
    sha: asset?.startsWith('sha256:') ? asset.slice(7) : undefined,
    bytes: info?.size ?? null,
    w: media.w ?? null,
    h: media.h ?? null,
    duration: media.duration ?? null,
    text: (n.kind === 'note' || n.kind === 'text') && (!asset || n.text) ? n.text : null,
    node: n.id,
    sample: sampleImage(),
  };
}

/** The first image on the canvas (a LUT's before / after). */
function sampleImage(): string | undefined {
  const { nodes, app } = getState();
  const img = nodes.find((x) => x.kind === 'image' && typeof x.props.asset === 'string');
  return (img && app?.assetUrl(img.props.asset as string)) || undefined;
}

/** Previewable nodes in reading order. */
export function previewOrder(nodes: NodeView[] = getState().nodes): string[] {
  const rects = worldRects(nodes);
  return nodes
    .filter((n) => previewable(n) && n.kind !== 'note')
    .map((n) => ({ id: n.id, r: rects.get(n.id)! }))
    .sort((a, b) => {
      const sameRow = Math.abs(a.r[1] - b.r[1]) < Math.min(a.r[3], b.r[3]) / 2;
      return sameRow ? a.r[0] - b.r[0] : a.r[1] - b.r[1];
    })
    .map((v) => v.id);
}

export function openPreview(id: string) {
  setState({ preview: id });
}

export function closePreview() {
  setState({ preview: null });
}

export function stepPreview(dir: 1 | -1) {
  const cur = getState().preview;
  const list = previewOrder();
  if (!cur || list.length < 2) return;
  const i = list.indexOf(cur);
  const next = list[(i + dir + list.length) % list.length];
  setState({ preview: next });
  getState().app?.setSelection([next]);
}

/** Keys the open viewer handles itself (set by the viewer: audio transport, PDF paging,
 *  image zoom). Returns true if handled. */
export const viewerKeys: { handle: ((e: KeyboardEvent) => boolean) | null } = { handle: null };

/** Keys while the canvas preview is open (it owns the keyboard). */
export function previewKey(e: KeyboardEvent): boolean {
  if (e.metaKey || e.ctrlKey) return false;
  if (e.key === 'Escape') return closePreview(), true;
  if (viewerKeys.handle?.(e)) return true;
  if (e.key === ' ') return closePreview(), true;
  if (e.key === 'ArrowLeft') return stepPreview(-1), true;
  if (e.key === 'ArrowRight') return stepPreview(1), true;
  return false;
}
