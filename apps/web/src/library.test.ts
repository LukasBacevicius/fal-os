import { describe, expect, it } from 'vitest';
import { justify } from './components/Library';
import type { LibItem } from './library';

const item = (w: number, h: number, kind: LibItem['kind'] = 'image') => ({ sha: `${w}x${h}${Math.random()}`, kind, w, h }) as LibItem;

describe('justified grid', () => {
  it('fills rows to the width and keeps the last row at the target height', () => {
    const items = Array.from({ length: 50 }, (_, i) => item(i % 3 ? 1600 : 900, i % 3 ? 900 : 1600));
    const L = justify(items, 1000, 200);
    for (const r of L.rows.slice(0, -1)) {
      const last = r.end - 1;
      expect(L.x[last] + L.w[last]).toBeCloseTo(1000, 0);
      expect(r.h).toBeLessThanOrEqual(320);
    }
    expect(justify(items.slice(0, 1), 1000, 200).rows[0].h).toBe(200);
    expect(L.height).toBeGreaterThan(0);
  });

  it('lays out 5k items fast', () => {
    const items = Array.from({ length: 5000 }, (_, i) => item(100 + (i % 7) * 50, 100 + (i % 5) * 40));
    const t = performance.now();
    const L = justify(items, 1400, 200);
    expect(performance.now() - t).toBeLessThan(50);
    expect(L.rows.reduce((n, r) => n + r.end - r.start, 0)).toBe(5000);
  });
});
