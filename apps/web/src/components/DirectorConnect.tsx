import { useEffect, useState } from 'react';
import { setState, toast, useStore, type DirectorTarget } from '../store';
import { hostSync } from '../sync';

// One click to use fal-os from the user's own Claude Code, Claude desktop app and Codex (the
// "director" setup, host `connect.rs`): registers the fal-os MCP server and installs the fal-os
// skill. The file list below is what the click changes; nothing is written before it.

const AFTER: Record<DirectorTarget['id'], string> = {
  'claude-code': 'restart Claude Code (or /mcp). desktop control: /mcp → computer-use → enable',
  'claude-desktop': 'quit and reopen the Claude app. desktop control: Settings → This computer',
  codex: 'start a new Codex session. desktop control is in the Codex / ChatGPT app (Plugins → Computer Use), not the CLI',
};

const STATE: Record<DirectorTarget['state'], string> = {
  connected: 'connected',
  outdated: 'outdated',
  'not-installed': 'not installed',
};

export function DirectorConnect() {
  const d = useStore((s) => s.director);
  const [picked, setPicked] = useState<Record<string, boolean> | null>(null);
  const [confirmOff, setConfirmOff] = useState(false);
  const tilde = (p: string) => (d?.home && p.startsWith(d.home + '/') ? '~' + p.slice(d.home.length) : p);

  useEffect(() => {
    hostSync()?.director('status');
  }, []);

  // Default selection: clients that are installed here.
  useEffect(() => {
    if (d && !picked && d.targets.length) setPicked(Object.fromEntries(d.targets.map((t) => [t.id, t.detected])));
  }, [d, picked]);

  if (!d) return <div className="faint">checking your agents…</div>;
  const sel = d.targets.filter((t) => picked?.[t.id]);
  const writes = sel.flatMap((t) => t.files.filter((f) => f.connect !== 'none' && !f.problem).map((f) => ({ t, f })));
  const removes = sel.flatMap((t) => t.files.filter((f) => f.disconnect !== 'none').map((f) => ({ t, f })));

  const send = (type: 'connect' | 'disconnect') => {
    if (!hostSync()?.director(type, sel.map((t) => t.id))) return toast('not connected to the host');
    setState((s) => ({ director: s.director ? { ...s.director, busy: true } : s.director }));
    setConfirmOff(false);
  };
  const label = sel.length === d.targets.length && sel.length > 1 ? 'all' : sel.map((t) => t.label).join(' + ');

  return (
    <div className="director">
      {d.targets.map((t) => (
        <div key={t.id} className="director-row">
          <button
            className="link"
            role="checkbox"
            aria-checked={!!picked?.[t.id]}
            onClick={() => setPicked({ ...(picked ?? {}), [t.id]: !picked?.[t.id] })}
          >
            {picked?.[t.id] ? '[x]' : '[ ]'} {t.label}
          </button>
          <span className={t.state === 'connected' ? 'dim' : 'faint'}>
            {STATE[t.state]}
            {!t.detected && t.state === 'not-installed' ? ' · app not found' : ''}
            {t.state !== 'not-installed' ? ` · ${t.transport}` : ''}
          </span>
        </div>
      ))}
      {writes.length > 0 ? (
        <div className="director-files">
          <div className="faint">[connect] writes (keeps everything else in each file):</div>
          {writes.map(({ t, f }) => (
            <div key={f.path} className="faint">
              {'  '}
              {f.connect} {tilde(f.path)} <span className="dim">({f.what === 'mcp' ? `fal-os MCP server, ${t.transport}` : 'fal-os skill'})</span>
            </div>
          ))}
        </div>
      ) : sel.length > 0 ? (
        <div className="faint">selected agents are up to date.</div>
      ) : null}
      {sel.flatMap((t) => t.files.filter((f) => f.problem)).map((f) => (
        <div key={f.path} className="err">{tilde(f.path)}: {f.problem}</div>
      ))}
      <div className="setup-row">
        <button className="btn" disabled={!writes.length || d.busy} onClick={() => send('connect')}>
          {d.busy ? 'working…' : `connect ${label}`.trim()}
        </button>
        {removes.length > 0 && !confirmOff && (
          <button className="link dim" disabled={d.busy} onClick={() => setConfirmOff(true)}>
            [disconnect]
          </button>
        )}
        <button className="link dim" onClick={() => hostSync()?.director('status')}>[recheck]</button>
      </div>
      {confirmOff && removes.length > 0 && (
        <div className="director-files">
          <div className="faint">[disconnect] removes only what connect added:</div>
          {removes.map(({ f }) => (
            <div key={f.path} className="faint">
              {'  '}
              {f.what === 'mcp' ? `the fal-os entry in ${tilde(f.path)}` : `${tilde(f.path)}`}
            </div>
          ))}
          <div className="setup-row">
            <button className="btn" disabled={d.busy} onClick={() => send('disconnect')}>remove</button>
            <button className="link dim" onClick={() => setConfirmOff(false)}>[keep]</button>
          </div>
        </div>
      )}
      {d.error && <div className="err">{d.error}</div>}
      {!!d.changed?.length && (
        <div className="director-files">
          <div className="dim">changed:</div>
          {d.changed.map((p) => (
            <div key={p} className="faint">{'  '}{tilde(p)}</div>
          ))}
        </div>
      )}
      {d.targets
        .filter((t) => t.state === 'connected' && picked?.[t.id])
        .map((t) => (
          <div key={t.id} className="faint">{t.label}: {AFTER[t.id]}</div>
        ))}
    </div>
  );
}
