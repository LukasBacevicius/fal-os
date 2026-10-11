// Figma paste: the plan shape (crates/figma `Plan`) and its pure mapping to canvas commands.

import type { RasterSpec } from './raster';

export interface TextStyle {
  characters: string; fontSize: number; color?: string; fontFamily: string; fontWeight: number; italic: boolean;
  lineHeight?: number; letterSpacing: number; align: string; valign: string; wrap: boolean;
}

export interface PlanNode {
  parent: number | null;
  kind: 'frame' | 'group' | 'text' | 'image' | 'raster' | 'note';
  name: string;
  x: number; y: number; w: number; h: number;
  fill?: string; radius?: number; image?: string;
  text?: TextStyle;
  raster?: RasterSpec;
}

export interface FigmaPlan {
  fileKey: string | null;
  size: [number, number];
  nodes: PlanNode[];
  images: Record<string, string>;
  warnings: string[];
  stats: Record<string, number>;
}

/** Whether clipboard HTML carries a Figma *scene* (not e.g. a Copy-as-PNG side payload). */
export function isFigmaScene(html: string): boolean {
  if (!html.includes('(figma)') || !html.includes('(figmeta)')) return false;
  const m = /\(figmeta\)([A-Za-z0-9+/=\s]+)/.exec(html);
  if (!m) return true;
  try {
    const meta = JSON.parse(atob(m[1].replace(/\s/g, '')));
    return !meta.dataType || meta.dataType === 'scene';
  } catch {
    return true;
  }
}

const r2 = (v: number) => Math.round(v * 100) / 100;

/** Pure: the plan as `CanvasApp.apply` commands, roots offset to `origin` (world). */
export function planCommands(plan: FigmaPlan, origin: [number, number], newId: () => string) {
  const ids = plan.nodes.map(() => newId());
  const roots: string[] = [];
  const cmds = plan.nodes.map((n, i) => {
    const root = n.parent === null;
    if (root) roots.push(ids[i]);
    const props: Record<string, unknown> = { x: r2(n.x + (root ? origin[0] : 0)), y: r2(n.y + (root ? origin[1] : 0)), w: r2(n.w), h: r2(n.h) };
    let kind: string = n.kind;
    let text: string | undefined;
    switch (n.kind) {
      case 'frame':
        if (n.fill) props.fill = n.fill;
        if (n.radius) props.radius = r2(n.radius);
        break;
      case 'image':
        if (n.radius) props.radius = r2(n.radius);
        break;
      case 'raster':
        kind = 'image';
        props.transparent = true;
        break;
      case 'text': {
        const t = n.text!;
        text = t.characters;
        Object.assign(props, { fontSize: t.fontSize, color: t.color ?? '#000000', font: t.fontFamily, weight: t.fontWeight, align: t.align, valign: t.valign });
        if (t.lineHeight) props.lineHeight = t.lineHeight;
        if (t.letterSpacing) props.letterSpacing = t.letterSpacing;
        if (t.italic) props.italic = true;
        if (!t.wrap) props.nowrap = true;
        break;
      }
      case 'note':
        text = n.text?.characters ?? '';
        if (n.fill) props.color = n.fill;
        break;
    }
    return { op: 'create', id: ids[i], kind, parent: root ? null : ids[n.parent!], name: n.name, props, ...(text !== undefined ? { text } : {}) };
  });
  return { cmds, ids, roots };
}
