import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

// The store schedules fades with window timers.
beforeAll(() => {
  (globalThis as any).window ??= globalThis;
});

const load = async () => await import('./store');

describe('notice stack', () => {
  afterEach(() => vi.useRealTimers());

  it('successes fade, failures stay, ongoing lines update in place and resolve', async () => {
    vi.useFakeTimers();
    const { getState, notify, resolveNotice, toast, dismissNotice } = await load();
    toast('exported 3 files');
    toast('Upload to host failed: 500');
    notify('reconnecting to host…', { key: 'conn', level: 'ongoing' });
    notify('not saved to disk yet · retrying', { key: 'save', level: 'ongoing' });
    notify('not saved to disk yet · retrying (Permission denied)', { key: 'save', level: 'ongoing' });
    let s = getState().notices;
    expect(s.map((n) => n.level)).toEqual(['ok', 'error', 'ongoing', 'ongoing']);
    expect(s.find((n) => n.key === 'save')?.text).toContain('Permission denied');

    vi.advanceTimersByTime(3500);
    s = getState().notices;
    expect(s.some((n) => n.text === 'exported 3 files')).toBe(false);
    expect(s.some((n) => n.level === 'error')).toBe(true);

    resolveNotice('conn');
    expect(getState().notices.find((n) => n.key === 'conn')?.text).toBe('back in sync');
    vi.advanceTimersByTime(3500);
    expect(getState().notices.some((n) => n.key === 'conn')).toBe(false);
    expect(getState().notices.some((n) => n.key === 'save')).toBe(true);

    for (const n of getState().notices) dismissNotice(n.key);
    expect(getState().notices).toEqual([]);
  });

  it('resolving a condition that never showed does nothing', async () => {
    const { getState, resolveNotice } = await load();
    resolveNotice('nothing-here');
    expect(getState().notices.some((n) => n.key === 'nothing-here')).toBe(false);
  });
});

describe('handshake', () => {
  it('a different sync protocol means the ui is out of date', async () => {
    (globalThis as any).location ??= { search: '', hash: '' };
    const { protocolMismatch, PROTOCOL } = await import('./sync');
    expect(protocolMismatch(PROTOCOL)).toBe(false);
    expect(protocolMismatch(PROTOCOL + 1)).toBe(true);
    expect(protocolMismatch(undefined)).toBe(false);
  });
});
