import { describe, expect, it } from 'vitest';
import { isFigmaScene, planCommands, type FigmaPlan } from './figmaPlan';

const b64 = (s: string) => Buffer.from(s).toString('base64');
const html = (meta: object) => `<meta charset="utf-8"><span data-metadata="<!--(figmeta)${b64(JSON.stringify(meta))}(/figmeta)-->"></span><span data-buffer="<!--(figma)ZmlnLWtpd2k=(/figma)-->"></span>`;

describe('figma paste', () => {
  it('recognizes scene payloads only', () => {
    expect(isFigmaScene(html({ fileKey: 'k', dataType: 'scene' }))).toBe(true);
    expect(isFigmaScene(html({ fileKey: 'k' }))).toBe(true);
    expect(isFigmaScene(html({ fileKey: 'k', dataType: 'png' }))).toBe(false);
    expect(isFigmaScene('<b>hello</b>')).toBe(false);
  });

  it('maps a plan to one batch of create commands, roots at the origin', () => {
    const plan: FigmaPlan = {
      fileKey: 'k',
      size: [400, 300],
      images: { abc: 'https://example.com/a' },
      warnings: [],
      stats: {},
      nodes: [
        { parent: null, kind: 'frame', name: 'Board', x: 0, y: 0, w: 400, h: 300, fill: '#ffffff', radius: 12 },
        { parent: 0, kind: 'text', name: 'Title', x: 20, y: 20, w: 200, h: 30, text: { characters: 'Hi', fontSize: 24, color: '#000000', fontFamily: 'Inter', fontWeight: 700, italic: false, lineHeight: 28.8, letterSpacing: 0, align: 'left', valign: 'top', wrap: false } },
        { parent: 0, kind: 'image', name: 'Photo', x: 20, y: 60, w: 160, h: 90, image: 'abc', radius: 8 },
        { parent: 0, kind: 'raster', name: 'Icon', x: 200, y: 60, w: 20, h: 20, raster: { ops: [], images: [] } },
        { parent: null, kind: 'group', name: 'Loose', x: 410, y: 0, w: 10, h: 10 },
      ],
    };
    let n = 0;
    const { cmds, ids, roots } = planCommands(plan, [1000.5, -50], () => `id${n++}`);
    expect(ids).toEqual(['id0', 'id1', 'id2', 'id3', 'id4']);
    expect(roots).toEqual(['id0', 'id4']);
    expect(cmds[0]).toEqual({ op: 'create', id: 'id0', kind: 'frame', parent: null, name: 'Board', props: { x: 1000.5, y: -50, w: 400, h: 300, fill: '#ffffff', radius: 12 } });
    // Children stay parent-relative.
    expect(cmds[1]).toMatchObject({ kind: 'text', parent: 'id0', text: 'Hi', props: { x: 20, y: 20, fontSize: 24, color: '#000000', font: 'Inter', weight: 700, lineHeight: 28.8, nowrap: true } });
    // Images start as placeholders (no asset); rasters are transparent images.
    expect(cmds[2]).toMatchObject({ kind: 'image', props: { radius: 8 } });
    expect((cmds[2].props as Record<string, unknown>).asset).toBeUndefined();
    expect(cmds[3]).toMatchObject({ kind: 'image', props: { transparent: true, w: 20, h: 20 } });
    expect(cmds[4]).toMatchObject({ kind: 'group', parent: null, props: { x: 1410.5, y: -50 } });
  });
});
