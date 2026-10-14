// Progress only when an operation is slow (docs/research/brilliant-b102.md §10): an instant
// paste, import, project switch or export shows nothing; one that takes longer than SLOW_MS
// shows an `ongoing` line (`pasting…`) that ends with a brief done line, and never a "done"
// over an error: when anything failed meanwhile, the error line is the last word.

import { dismissNotice, errorsSoFar, notify } from './store';

export const SLOW_MS = 400;

let depth = 0;
let seq = 0;

export interface ProgressOpts<T> {
  /** The done line (shown only if the progress line was): text, or null for none. */
  done?: (r: T) => string | null;
  key?: string;
}

/** Runs `work` with slow-only progress. Nested calls (a paste that imports files) show only
 *  the outer line. Errors propagate to the caller, who names them. */
export async function withProgress<T>(label: string, work: () => Promise<T>, o: ProgressOpts<T> = {}): Promise<T> {
  if (depth > 0) return work();
  depth++;
  const key = o.key ?? `progress:${++seq}`;
  const errors = errorsSoFar();
  let shown = false;
  const t = window.setTimeout(() => {
    shown = true;
    notify(`${label}…`, { key, level: 'ongoing' });
  }, SLOW_MS);
  try {
    const r = await work();
    window.clearTimeout(t);
    if (shown) {
      const done = errorsSoFar() === errors ? o.done?.(r) : null;
      if (done) notify(done, { key, level: 'ok' });
      else dismissNotice(key);
    }
    return r;
  } catch (e) {
    window.clearTimeout(t);
    dismissNotice(key);
    throw e;
  } finally {
    depth--;
  }
}

/** Slow-only progress for something that ends elsewhere (a project switch ends when the host's
 *  snapshot is in): `start` arms it, `end` resolves it. */
export function slowLine(key: string) {
  let t = 0;
  let shown = false;
  return {
    start(label: string) {
      window.clearTimeout(t);
      shown = false;
      t = window.setTimeout(() => {
        shown = true;
        notify(`${label}…`, { key, level: 'ongoing' });
      }, SLOW_MS);
    },
    end(done: string | null = null) {
      window.clearTimeout(t);
      if (!shown) return;
      shown = false;
      if (done) notify(done, { key, level: 'ok' });
      else dismissNotice(key);
    },
  };
}
