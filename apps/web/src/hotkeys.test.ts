import { describe, expect, it } from 'vitest';
import { phys } from './hotkeys';
import { duplicateOffset } from './geometry';

describe('layout-independent shortcuts', () => {
  it('matches the character on any layout', () => {
    expect(phys({ key: '/', code: 'Digit7' }, '/', 'Slash')).toBe(true); // German ⇧7
    expect(phys({ key: '[', code: 'BracketLeft' }, '[', 'BracketLeft')).toBe(true);
  });
  it('falls back to the US physical key for characters without a meaning', () => {
    expect(phys({ key: 'ü', code: 'BracketLeft' }, '[', 'BracketLeft')).toBe(true); // German ⌘ü = ⌘[
    expect(phys({ key: '#', code: 'Backslash' }, '\\', 'Backslash')).toBe(true); // German #
    expect(phys({ key: '§', code: 'Slash' }, '?', 'Slash')).toBe(true); // AZERTY ⇧!
  });
  it("never steals a key that is a shortcut itself", () => {
    expect(phys({ key: '-', code: 'Slash' }, '/', 'Slash')).toBe(false); // German -: zoom out
    expect(phys({ key: 'k', code: 'Slash' }, '/', 'Slash')).toBe(false);
    expect(phys({ key: 'ü', code: 'BracketRight' }, '[', 'BracketLeft')).toBe(false);
  });
});

describe('duplicate offset', () => {
  it('is 24, 24 first, then repeats the last move', () => {
    expect(duplicateOffset(['a'], null, [0, 0, 10, 10])).toEqual([24, 24]);
    // The copy (b) of a at 0,0 was moved to 200,0: the next copy goes 200 further.
    expect(duplicateOffset(['b'], { copies: ['b'], from: [0, 0, 10, 10] }, [200, 0, 10, 10])).toEqual([200, 0]);
    // Another selection: back to the default.
    expect(duplicateOffset(['c'], { copies: ['b'], from: [0, 0, 10, 10] }, [200, 0, 10, 10])).toEqual([24, 24]);
  });
});
