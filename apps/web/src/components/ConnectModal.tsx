import { useEffect, useRef, useState } from 'react';
import { getState, setState, toast, useStore } from '../store';
import { hostSync } from '../sync';
import { DirectorConnect } from './DirectorConnect';

interface ClientDef {
  id: string;
  label: string;
  /** `agent.register` client id; null = manual only. */
  register: string | null;
  config: string;
  snippet: (url: string, token: string) => string;
  restart: string;
}

const json = (url: string, token: string, typed: boolean) =>
  JSON.stringify({ mcpServers: { 'fal-os': { ...(typed ? { type: 'http' } : {}), url, headers: { Authorization: `Bearer ${token}` } } } }, null, 2);

const CLIENTS: ClientDef[] = [
  {
    id: 'claude', label: 'Claude Code', register: 'claude', config: '~/.claude.json',
    snippet: (u, t) => `claude mcp add --transport http fal-os ${u} --header "Authorization: Bearer ${t}"`,
    restart: 'Restart Claude Code (or run /mcp) so it picks up fal-os.',
  },
  {
    id: 'codex', label: 'Codex', register: 'codex', config: '~/.codex/config.toml',
    snippet: (u, t) => `[mcp_servers.fal-os]\nurl = "${u}"\nhttp_headers = { Authorization = "Bearer ${t}" }`,
    restart: 'Start a new Codex session.',
  },
  {
    id: 'cursor', label: 'Cursor', register: 'cursor', config: '~/.cursor/mcp.json',
    snippet: (u, t) => json(u, t, false),
    restart: 'Reload Cursor (Settings → MCP shows fal-os).',
  },
  {
    id: 'gemini', label: 'Gemini', register: 'gemini', config: '~/.gemini/settings.json',
    snippet: (u, t) => json(u, t, false).replace('"url"', '"httpUrl"'),
    restart: 'Restart the Gemini CLI.',
  },
  {
    id: 'opencode', label: 'OpenCode', register: 'opencode', config: '~/.config/opencode/opencode.json',
    snippet: (u, t) => JSON.stringify({ mcp: { 'fal-os': { type: 'remote', url: u, headers: { Authorization: `Bearer ${t}` } } } }, null, 2),
    restart: 'Restart OpenCode.',
  },
  {
    id: 'other', label: 'Other', register: null, config: 'your client’s MCP config',
    snippet: (u, t) => `URL:    ${u}\nHeader: Authorization: Bearer ${t}\nTransport: Streamable HTTP (or stdio: falos-host mcp)`,
    restart: 'Restart your agent.',
  },
];

const PROMPTS = [
  'In fal-os, make a moodboard for a neon Tokyo night shoot',
  'In fal-os, look at my selection and generate 4 variations of it',
  'In fal-os, lay out a storyboard for a 15s headphones ad in a frame',
];

/** Whether a host `client` key (normalized) belongs to a tab. */
const matches = (tab: string, client: string) =>
  tab === 'other' ? !CLIENTS.some((c) => c.id !== 'other' && client.toLowerCase().includes(c.id)) : client.toLowerCase().includes(tab);

function copy(text: string, what: string) {
  navigator.clipboard?.writeText(text).then(() => toast(`${what} copied`), () => toast('copy failed'));
}

/** "Connect your agent" (UX-V1 §7): a tab per client with three steps whose checkmarks come
 *  from real host events: connected when its session appears, done on its first write (then
 *  the dialog closes and the canvas shows the result). */
export function ConnectModal() {
  const connect = useStore((s) => s.connect);
  // First tab: one click for the user's own Claude Code, Claude app and Codex (MCP + skill).
  const [tab, setTab] = useState('director');
  const [manual, setManual] = useState(false);
  const [busy, setBusy] = useState(false);
  const panel = useRef<HTMLDivElement>(null);
  const def = CLIENTS.find((c) => c.id === tab) ?? CLIENTS[0];
  const TABS = ['director', ...CLIENTS.map((c) => c.id)];
  const info = hostSync()?.info;
  const url = info ? `http://${info.addr}/mcp` : '';
  const token = info?.token ?? '';

  const close = () => setState({ overlay: null });

  // First write from any client: close, show the result, say it worked.
  useEffect(() => {
    const on = (e: Event) => {
      const m = (e as CustomEvent).detail as { client: string; nodes: string[] };
      close();
      // The doc update with the new nodes may land just after the event: retry briefly.
      let tries = 0;
      const zoom = () => {
        const { app, nodes } = getState();
        const have = (m.nodes ?? []).filter((id) => nodes.some((n) => n.id === id));
        if (app && have.length) app.zoomToNodes(have);
        else if (++tries < 20) window.setTimeout(zoom, 100);
      };
      zoom();
      const name = CLIENTS.find((c) => matches(c.id, m.client))?.label ?? m.client;
      toast(`${name} is connected: it just wrote ${m.nodes?.length ?? 0} node${m.nodes?.length === 1 ? '' : 's'}`);
    };
    window.addEventListener('falos:first-write', on);
    return () => window.removeEventListener('falos:first-write', on);
  }, []);

  useEffect(() => {
    setBusy(false);
  }, [connect.registered]);

  useEffect(() => panel.current?.focus(), [tab]);

  const reg = def.register ? connect.registered[def.register] : undefined;
  const conn = Object.entries(connect.connected).find(([c]) => matches(tab, c));
  const wrote = Object.keys(connect.firstWrite).some((c) => matches(tab, c));
  const step1 = !!conn || !!reg?.ok;

  const register = () => {
    if (!def.register) return;
    if (!hostSync()?.registerAgent(def.register)) return toast('not connected to the host');
    setBusy(true);
    window.setTimeout(() => setBusy(false), 6000);
  };

  const ix = TABS.indexOf(tab);
  return (
    <div className="modal" onPointerDown={close}>
      <div
        className="connect"
        onPointerDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === 'Escape') close();
          else if ((e.key === 'ArrowDown' || e.key === 'ArrowUp') && (e.target as HTMLElement).tagName !== 'TEXTAREA') {
            e.preventDefault();
            setTab(TABS[(ix + (e.key === 'ArrowDown' ? 1 : TABS.length - 1)) % TABS.length]);
          }
        }}
      >
        <div className="connect-tabs">
          <div className="dim connect-title">connect your agent</div>
          <div className={`connect-tab ${tab === 'director' ? 'on' : ''}`} onClick={() => setTab('director')}>
            Claude + Codex
          </div>
          {CLIENTS.map((c) => {
            const ok = Object.keys(connect.connected).some((k) => matches(c.id, k));
            return (
              <div key={c.id} className={`connect-tab ${c.id === tab ? 'on' : ''}`} onClick={() => setTab(c.id)}>
                {c.label}{ok ? ' ✓' : ''}
              </div>
            );
          })}
        </div>
        <div className="connect-body" ref={panel} tabIndex={-1}>
          {tab === 'director' ? (
            <>
              <div className="step">
                <div className="step-head">use fal-os from your own Claude and Codex</div>
                <div className="step-body faint">
                  they direct: generate and lay out here, hand exported files to other apps (editors via computer use) and bring results back with lineage.
                  one click adds the fal-os MCP server and the fal-os skill to each.
                </div>
              </div>
              <div className="step-body">
                <DirectorConnect />
              </div>
              <div className="step">
                <div className="step-head">{Object.keys(connect.firstWrite).length ? '[x]' : '[ ]'} then try a prompt</div>
                {PROMPTS.map((p) => (
                  <div key={p} className="step-body prompt-row">
                    <span className="prompt-text">“{p}”</span>
                    <button className="link dim" onClick={() => copy(p, 'prompt')}>[copy]</button>
                  </div>
                ))}
              </div>
            </>
          ) : (
          <>
          <div className="step">
            <div className="step-head">{step1 ? '[x]' : '[ ]'} 1. connect</div>
            {def.register ? (
              <div className="step-body">
                <button className="btn" onClick={register} disabled={busy}>{busy ? 'adding…' : reg?.ok ? `added to ${def.label}` : `add to ${def.label}`}</button>
                <div className="faint">writes {reg?.path ?? def.config} (keeps your other settings)</div>
                {reg && !reg.ok && <div className="err">couldn’t update the config: {reg.error ?? 'unknown error'}</div>}
              </div>
            ) : (
              <div className="step-body faint">add fal-os to your client’s MCP servers:</div>
            )}
            <div className="step-body">
              {def.register && (
                <button className="link dim" onClick={() => setManual(!manual)}>{manual ? '▾' : '▸'} manual setup</button>
              )}
              {(manual || !def.register) && (
                <div className="snippet">
                  <pre>{def.snippet(url || 'http://127.0.0.1:<port>/mcp', token ? `${token.slice(0, 4)}…${token.slice(-4)}` : '<token>')}</pre>
                  <button className="link dim" disabled={!token} onClick={() => copy(def.snippet(url, token), 'snippet')}>[copy]</button>
                </div>
              )}
            </div>
            {conn && <div className="step-body dim">connected as {conn[1].agentName}</div>}
          </div>
          <div className="step">
            <div className="step-head">{conn ? '[x]' : '[ ]'} 2. restart your agent</div>
            <div className="step-body faint">{def.restart}</div>
          </div>
          <div className="step">
            <div className="step-head">{wrote ? '[x]' : '[ ]'} 3. try a prompt</div>
            {PROMPTS.map((p) => (
              <div key={p} className="step-body prompt-row">
                <span className="prompt-text">“{p}”</span>
                <button className="link dim" onClick={() => copy(p, 'prompt')}>[copy]</button>
              </div>
            ))}
            <div className="step-body faint">{conn ? 'waiting for its first change on the canvas…' : 'its first change shows up here'}</div>
          </div>
          </>
          )}
        </div>
      </div>
    </div>
  );
}
