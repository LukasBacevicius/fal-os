import { describe, expect, it } from 'vitest';
import { anchorLabel, timeAgo, timecode } from './format';

describe('format', () => {
  it('timecode', () => {
    expect(timecode(0)).toBe('0:00.00');
    expect(timecode(72.5)).toBe('1:12.50');
  });

  it('anchor labels use normalized coordinates', () => {
    expect(anchorLabel({ kind: 'point', x: 0.25, y: 0.5 })).toBe('Point 25%, 50%');
    expect(anchorLabel({ kind: 'box', x: 0, y: 0, w: 0.5, h: 0.25 })).toBe('Region 50% × 25%');
    expect(anchorLabel({ kind: 'frame', t: 4.2, region: { kind: 'box', x: 0, y: 0, w: 1, h: 1 } })).toBe('Frame 0:04.20 · region');
  });

  it('timeAgo', () => {
    const now = 1_000_000_000;
    expect(timeAgo(now - 10_000, now)).toBe('just now');
    expect(timeAgo(now - 5 * 60_000, now)).toBe('5m');
    expect(timeAgo(now - 3 * 3_600_000, now)).toBe('3h');
  });
});
