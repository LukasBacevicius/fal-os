import { useEffect, useRef, useState } from 'react';
import { useStore, type State } from '../store';
import { DRIVERS, setDriver } from '../chatActions';
import { MOD } from '../keys';

type Driver = (typeof DRIVERS)[number];

/** The capability each driver needs (CHAT-PROTOCOL.md, Capabilities). */
const NEEDS: Record<Driver, string> = { opus: 'claude', fal: 'fal-agent', codex: 'codex' };
const ROLE: Record<Driver, string> = { opus: 'claude director', fal: 'fal agent', codex: 'openai codex' };

type Caps = { missing: { name: string; fix?: string }[] } | null;
let memo: { threads: unknown; active: string | null; caps: Caps } | null = null;

/** The latest `capabilities` event: the active chat's first, else any chat's (memoized on the
 *  threads object, so store updates elsewhere cost nothing). */
function latestCaps(s: State): Caps {
  const { threads, active } = s.chat;
  if (memo && memo.threads === threads && memo.active === active) return memo.caps;
  const caps = scanCaps(threads, active);
  memo = { threads, active, caps };
  return caps;
}

function scanCaps(threads: State['chat']['threads'], active: string | null): Caps {
  const ids = active ? [active, ...Object.keys(threads).filter((id) => id !== active)] : Object.keys(threads);
  for (const id of ids) {
    const ev = threads[id]?.events;
    if (!ev) continue;
    for (let i = ev.length - 1; i >= 0; i--) {
      const e = ev[i] as { kind: string; missing?: unknown };
      if (e.kind === 'capabilities') {
        const missing = (Array.isArray(e.missing) ? e.missing : []).map((m: any) =>
          typeof m === 'string' ? { name: m } : { name: String(m?.name ?? ''), fix: m?.fix ? String(m.fix) : undefined });
        return { missing };
      }
    }
  }
  return null;
}

/** Why a driver can't run right now (null = available or unknown). */
function reasonOf(caps: Caps, d: Driver): string | null {
  const m = caps?.missing.find((x) => x.name === NEEDS[d]);
  if (!m) return null;
  return m.fix ? m.fix.replace(/`/g, '') : 'unavailable';
}

/** The composer's driver dropdown: `opus ▾` opens a mono list of the drivers (upwards, the
 *  composer sits at the bottom). ↑/↓ move, ⏎ / space pick, esc / tab close, a first letter
 *  jumps to that driver. Unavailable drivers (from the latest `capabilities` event) are dim
 *  with the reason, and can still be picked. ⌘. keeps cycling (keys.ts). */
export function DriverPicker() {
  const driver = useStore((s) => s.driver);
  const [open, setOpen] = useState(false);
  const [hi, setHi] = useState(0);
  const btn = useRef<HTMLButtonElement>(null);
  const box = useRef<HTMLSpanElement>(null);
  // One string per driver ('' = available), so the selector stays a cheap primitive.
  const why = useStore((s) => { const c = latestCaps(s); return DRIVERS.map((d) => reasonOf(c, d) ?? '').join('\u0001'); }).split('\u0001');

  const show = () => {
    setHi(Math.max(0, DRIVERS.indexOf(driver)));
    setOpen(true);
    btn.current?.focus(); // WebKit doesn't focus buttons on click
  };
  const close = (refocus: 'button' | 'chat' | null) => {
    setOpen(false);
    if (refocus === 'button') btn.current?.focus();
    if (refocus === 'chat') window.dispatchEvent(new Event('falos:focus-chat'));
  };
  const pick = (d: Driver) => {
    setDriver(d);
    close('chat');
  };

  // A press anywhere else closes it.
  useEffect(() => {
    if (!open) return;
    const down = (e: PointerEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('pointerdown', down, true);
    return () => document.removeEventListener('pointerdown', down, true);
  }, [open]);

  const onKey = (e: React.KeyboardEvent) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return; // ⌘. and friends stay global
    const k = e.key;
    if (!open) {
      if (k === 'ArrowDown' || k === 'ArrowUp' || k === 'Enter' || k === ' ') {
        e.preventDefault();
        show();
      }
      return;
    }
    const n = DRIVERS.length;
    if (k === 'ArrowDown' || k === 'ArrowUp') {
      setHi((h) => (h + (k === 'ArrowDown' ? 1 : n - 1)) % n);
    } else if (k === 'Home' || k === 'End') {
      setHi(k === 'Home' ? 0 : n - 1);
    } else if (k === 'Enter' || k === ' ') {
      pick(DRIVERS[hi]);
    } else if (k === 'Escape') {
      close('button');
    } else if (k === 'Tab') {
      setOpen(false);
      return; // let focus move on
    } else if (/^[a-z]$/i.test(k)) {
      // Type-ahead: the next driver starting with that letter (repeats cycle among them).
      const l = k.toLowerCase();
      for (let i = 1; i <= n; i++) {
        const j = (hi + i) % n;
        if (DRIVERS[j].startsWith(l)) {
          setHi(j);
          break;
        }
      }
    } else {
      return;
    }
    e.preventDefault();
    e.stopPropagation(); // e.g. esc must not also stop the run
  };

  const current = why[DRIVERS.indexOf(driver)] || null;
  return (
    <span className="driver" ref={box} data-own-keys>
      <button
        ref={btn}
        className={`link driver-btn ${current ? 'faint' : ''}`}
        title={`agent for the next message (${MOD}. cycles)${current ? ` · unavailable: ${current}` : ''}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? 'driver-menu' : undefined}
        aria-activedescendant={open ? `driver-${DRIVERS[hi]}` : undefined}
        onClick={() => (open ? close(null) : show())}
        onKeyDown={onKey}
      >
        {driver} ▾
      </button>
      {open && (
        <div className="menu driver-menu" id="driver-menu" role="listbox" aria-label="agent for the next message">
          {DRIVERS.map((d, i) => {
            const off = why[i] || null;
            return (
              <div
                key={d}
                id={`driver-${d}`}
                role="option"
                aria-selected={d === driver}
                aria-disabled={off ? true : undefined}
                className={`menu-row ${i === hi ? 'on' : ''} ${off ? 'off' : ''}`}
                title={off ? `${d} unavailable — ${off}` : `${d}: ${ROLE[d]}`}
                onMouseEnter={() => setHi(i)}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => pick(d)}
              >
                <span className="driver-name">{d === driver ? '* ' : '  '}{d}</span>
                <span className="dim driver-why">{off ?? ROLE[d]}</span>
              </div>
            );
          })}
        </div>
      )}
    </span>
  );
}
