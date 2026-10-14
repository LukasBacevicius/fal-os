import { useState } from 'react';
import { copyText, setState, toast, useStore } from '../store';
import { plainError, type NextStep } from '../errors';
import { cancelHeld, releaseHeld } from '../hold';
import { isDesktop } from '../desktop';

function openUrl(url: string) {
  const invoke = (window as any).__TAURI_INTERNALS__?.invoke;
  const p = typeof invoke === 'function' ? invoke('plugin:opener|open_url', { url }) : null;
  if (p) p.catch(() => window.open(url, '_blank', 'noopener'));
  else window.open(url, '_blank', 'noopener');
}

/** Runs an error's next step. `cont` / `rerun`: what continue / rerun mean where it shows. */
export function runNext(n: NextStep, o: { cont?: () => void; rerun?: () => void } = {}) {
  switch (n.kind) {
    case 'setup':
      if (isDesktop()) setState({ overlay: 'setup' });
      else toast('put FAL_KEY=… in ~/.fal-os/fal.env, then restart the host');
      break;
    case 'url':
      openUrl(n.url);
      break;
    case 'copy':
      void copyText(n.text, `copied: ${n.text} (run it in a terminal)`);
      break;
    case 'continue':
      o.cont?.();
      break;
    case 'rerun':
      o.rerun?.();
      break;
  }
}

/** One plain sentence for an agent or generation error: `! <cause> · [<next step>]
 *  [details]`; the raw message behind [details]. `retried`: it was retried automatically. */
export function ErrorLine({ raw, cont, rerun, retried = false, className = 'row err-line' }: {
  raw: string;
  cont?: () => void;
  rerun?: () => void;
  retried?: boolean;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const pe = plainError(raw);
  const cause = pe.next ? pe.text.slice(0, pe.text.length - pe.next.label.length - 3) : pe.text;
  const n = pe.next;
  const actionable = n && (n.kind === 'continue' ? !!cont : n.kind === 'rerun' ? !!rerun : true);
  return (
    <div className={className}>
      ! {cause}
      {retried ? <span className="dim"> · retried once</span> : n && (
        <>
          {' · '}
          {actionable ? <button className="link" onClick={() => runNext(n, { cont, rerun })}>[{n.label}]</button> : <span className="dim">{n.label}</span>}
        </>
      )}
      {pe.raw && pe.raw !== cause && (
        <>
          {' '}
          <button className="link faint" onClick={() => setOpen(!open)} title="the original message">[details]</button>
        </>
      )}
      {open && <div className="err-details faint">{pe.raw}</div>}
    </div>
  );
}

const WHY: Record<string, string> = {
  host: 'host unreachable',
  runtime: 'agent runtime not connected',
  'fal-key': 'fal key missing',
  credits: 'out of credits',
  'claude-login': 'Claude login expired',
  'codex-login': 'Codex login expired',
};

/** The held message above the composer: why, and that it goes out by itself. */
export function HeldLine() {
  const held = useStore((s) => s.held);
  if (!held) return null;
  const preview = held.text.length > 60 ? `${held.text.slice(0, 59)}…` : held.text;
  const self = held.wall === 'host' || held.wall === 'runtime';
  const fix = plainError(WHY[held.wall] ?? '').next;
  return (
    <div className="held-line dim" title={held.text}>
      held · {WHY[held.wall] ?? held.wall} · {self ? 'sends when it is back' : 'sends when fixed'}
      {!self && fix && <> <button className="link" onClick={() => runNext(fix)}>[{fix.label}]</button></>}
      {' '}<button className="link" onClick={() => releaseHeld()} title="send it anyway">[send now]</button>
      {' '}<button className="link" onClick={cancelHeld} title="back into the input">[cancel]</button>
      <div className="faint held-text">&gt; {preview}</div>
    </div>
  );
}
