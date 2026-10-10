import type { Anchor, NodeKind, Status } from './types';

export function timeAgo(ms: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 45) return 'just now';
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.round(h / 24);
  if (d < 7) return `${d}d`;
  return new Date(ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

/** `12.345` → `0:12.34`. */
export function timecode(t: number): string {
  const m = Math.floor(t / 60);
  const s = t - m * 60;
  return `${m}:${s.toFixed(2).padStart(5, '0')}`;
}

export function anchorLabel(a: Anchor): string {
  const pct = (v: number) => `${Math.round(v * 100)}%`;
  switch (a.kind) {
    case 'point':
      return `Point ${pct(a.x)}, ${pct(a.y)}`;
    case 'box':
      return `Region ${pct(a.w)} × ${pct(a.h)}`;
    case 'frame':
      return `Frame ${timecode(a.t)}${a.region?.kind === 'box' ? ' · region' : ''}`;
    case 'time':
      return `At ${timecode(a.t)}`;
    case 'range':
      return `${timecode(a.t0)}–${timecode(a.t1)}`;
    case 'page':
      return `Page ${a.n}`;
    case 'text_range':
      return `Chars ${a.start}–${a.end}`;
    default:
      return a.kind;
  }
}

export const STATUS_LABEL: Record<Status, string> = { open: 'Open', in_progress: 'In progress', resolved: 'Resolved' };

export const KIND_LABEL: Record<string, string> = {
  image: 'Image', video: 'Video', audio: 'Audio', text: 'Text', model3d: '3D model', vector: 'Vector',
  document: 'Document', embed: 'Embed', timeline: 'Timeline', stream: 'Stream', generation: 'Generation',
  frame: 'Frame', group: 'Group', note: 'Note', connector: 'Connector',
};

export const kindLabel = (k: NodeKind) => KIND_LABEL[k] ?? (k.startsWith('style:') ? k.slice(6) : k);

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
