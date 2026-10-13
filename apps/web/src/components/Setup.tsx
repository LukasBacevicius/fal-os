import { useCallback, useEffect, useRef, useState } from 'react';
import { getState, setState, toast } from '../store';
import { createProject } from '../projects';
import { DirectorConnect } from './DirectorConnect';

// First-run setup for the packaged desktop app (apps/desktop, Tauri commands in
// src-tauri/src/sidecar.rs). Shown on launch until a fal key is stored, Claude Code is signed in
// and ffmpeg is there; later from the palette ("setup…"). Outside the packaged app it never
// opens. Nothing here needs Terminal: Claude Code, ffmpeg and Codex install from the buttons
// (fixed tasks in the shell with streamed output), sign-ins open the browser.

type Status = {
  packaged: boolean;
  error?: string;
  claude?: { path: string | null; loggedIn: boolean | null; subscription?: string | null };
  codex?: { path: string | null; signedIn: boolean };
  ffmpeg?: boolean;
  ffmpegPath?: string | null;
  falKey?: boolean;
  keySource?: 'keychain' | 'file' | 'none';
  extras?: { motion: boolean; hyperframes: boolean; chrome: boolean; skills: boolean };
  install?: { running?: boolean; step?: string; error?: string; done?: boolean } | null;
  installing?: boolean;
  diskFreeGb?: number | null;
  home?: string;
};

type KeyCheck = { ok: boolean; agent: { ok: boolean; reason?: string }; models: { ok: boolean; reason?: string } };

const TASKS = ['claude-install', 'claude-login', 'ffmpeg', 'codex-install', 'codex-login'] as const;
type TaskName = (typeof TASKS)[number];
type Task = { running: boolean; code: number | null; lines: string[]; url: string | null } | null;

const KEYS_URL = 'https://fal.ai/dashboard/keys';

const invoke = (cmd: string, args?: Record<string, unknown>): Promise<any> | null => {
  const f = (window as any).__TAURI_INTERNALS__?.invoke;
  return typeof f === 'function' ? f(cmd, args) : null;
};

/** In the system browser (tauri-plugin-opener; `opener:default` allows https). */
function openUrl(url: string) {
  const p = invoke('plugin:opener|open_url', { url });
  if (p) p.catch(() => window.open(url, '_blank'));
  else window.open(url, '_blank');
}

export async function setupStatus(): Promise<Status | null> {
  try {
    return (await invoke('setup_status')) ?? null;
  } catch {
    return null;
  }
}

const claudeReady = (s: Status) => !!s.claude?.path && s.claude.loggedIn !== false;
const required = (s: Status) => !!s.falKey && claudeReady(s) && !!s.ffmpeg;

/** Opened at launch (first run) rather than from the palette: "start" then opens a project. */
let firstRun = false;

/** Open the setup screen at launch when the packaged app still needs something required. */
export async function maybeOpenSetup() {
  const s = await setupStatus();
  if (s?.packaged && !required(s)) {
    firstRun = true;
    setState({ overlay: 'setup' });
  }
}

const box = (ok: boolean | null | undefined) => (ok ? '[x]' : '[ ]');

/** A task's last output lines while it runs, or its error when it failed. */
function TaskLog({ t, n = 4 }: { t: Task; n?: number }) {
  if (!t || (!t.running && t.code === 0) || !t.lines.length) return null;
  return <pre className="setup-log">{t.lines.slice(-n).join('\n')}</pre>;
}

/** "open at login" (tauri-plugin-autostart): a LaunchAgent that starts fal-os hidden, with
 *  just the menu bar item and the host running. Off by default. */
function OpenAtLogin() {
  const [on, setOn] = useState<boolean | null>(null);
  useEffect(() => {
    invoke('plugin:autostart|is_enabled')?.then((v) => setOn(!!v), () => setOn(null));
  }, []);
  if (on === null) return null;
  const toggle = () =>
    invoke(on ? 'plugin:autostart|disable' : 'plugin:autostart|enable')?.then(
      () => setOn(!on),
      (e) => toast(`open at login: ${e}`),
    );
  return (
    <div className="setup-row">
      <button className="link dim" onClick={toggle}>{box(on)} open at login</button>
      <span className="faint">starts hidden in the menu bar; agents keep running when the window is closed</span>
    </div>
  );
}

/** A sign-in that failed (not cancelled): the Terminal command as a last resort. */
function Fallback({ t, cmd }: { t: Task; cmd: string }) {
  if (!t || t.running || !t.code || t.code < 0) return null;
  return <div className="faint">didn't work? [sign in] again, or run `{cmd}` in Terminal.</div>;
}

export function Setup() {
  const [s, setS] = useState<Status | null>(null);
  const [tasks, setTasks] = useState<Partial<Record<TaskName, Task>>>({});
  const [key, setKey] = useState('');
  const [editKey, setEditKey] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [check, setCheck] = useState<KeyCheck | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  // Taken once: reopened from the palette later, the screen ends with "done", not "start".
  const [launch] = useState(() => {
    const v = firstRun;
    firstRun = false;
    return v;
  });
  const close = () => setState({ overlay: null });

  const refresh = useCallback(async () => {
    const v = await setupStatus();
    setS(v);
    return v;
  }, []);
  const pollTasks = useCallback(async () => {
    const all = await Promise.all(TASKS.map(async (t) => [t, ((await invoke('setup_task', { task: t })?.catch(() => null)) ?? null) as Task] as const));
    setTasks(Object.fromEntries(all));
  }, []);
  useEffect(() => {
    void refresh();
    void pollTasks();
    const esc = (e: KeyboardEvent) => e.key === 'Escape' && setState({ overlay: null });
    window.addEventListener('keydown', esc);
    return () => window.removeEventListener('keydown', esc);
  }, [refresh, pollTasks]);

  const running = TASKS.filter((t) => tasks[t]?.running);
  const anyRunning = running.length > 0;
  // Stream task output while something runs; re-read the status when a task ends.
  useEffect(() => {
    if (!anyRunning) return;
    const t = window.setInterval(() => void pollTasks(), 700);
    return () => window.clearInterval(t);
  }, [anyRunning, pollTasks]);
  const runningKey = running.join(',');
  const wasRunning = useRef('');
  useEffect(() => {
    if (wasRunning.current && runningKey !== wasRunning.current) void refresh();
    wasRunning.current = runningKey;
  }, [runningKey, refresh]);
  // While the motion tools install, and while something required is missing (the user may sign
  // in to Claude in the browser or finish an install), poll the status.
  const pending = !!s && (!required(s) || !!s.installing);
  useEffect(() => {
    if (!pending) return;
    const t = window.setInterval(() => void refresh(), s?.installing ? 1500 : 3000);
    return () => window.clearInterval(t);
  }, [pending, s?.installing, refresh]);
  useEffect(() => {
    if (!s?.falKey || editKey) input.current?.focus();
  }, [s?.falKey, editKey]);
  // Everything required is there: ⏎ starts (unless the user is typing somewhere).
  const finish = useRef<HTMLButtonElement>(null);
  const isReady = !!s && required(s);
  useEffect(() => {
    const a = document.activeElement;
    if (isReady && !(a instanceof HTMLInputElement || a instanceof HTMLTextAreaElement)) finish.current?.focus();
  }, [isReady]);

  const run = async (task: TaskName) => {
    setErr(null);
    try {
      await invoke('setup_run', { task });
    } catch (e) {
      setErr(String(e));
    }
    await pollTasks();
  };

  const installExtras = async () => {
    try {
      await invoke('install_extras');
    } catch (e) {
      setErr(String(e));
    }
    await refresh();
  };

  const saveKey = async (raw = key) => {
    const k = raw.trim();
    if (!k || busy) return;
    setErr(null);
    setBusy('checking');
    try {
      const c: KeyCheck = await invoke('check_fal_key', { key: k });
      setCheck(c);
      if (!c.models.ok) {
        setErr(`fal rejected this key (${c.models.reason ?? 'unknown'})`);
        return;
      }
      setBusy('saving');
      const where: string = await invoke('save_fal_key', { key: k });
      setKey('');
      setEditKey(false);
      toast(where === 'keychain' ? 'fal key saved in your Keychain' : 'fal key saved (~/.fal-os/fal.env)');
      const v = await refresh();
      // Nothing to click for the rest of the downloads: start them now, in the background.
      if (v?.packaged) {
        const x = v.extras;
        if (x && !(x.motion && x.hyperframes && x.chrome && x.skills) && !v.installing) void installExtras();
        if (!v.ffmpeg && !tasks.ffmpeg?.running) void run('ffmpeg');
      }
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  // Reads the clipboard only on this click (a user gesture), never on its own.
  const paste = async () => {
    try {
      const t = (await navigator.clipboard.readText()).trim();
      if (t) {
        setKey(t);
        void saveKey(t);
        return;
      }
    } catch {
      /* not allowed: fall through to ⌘V */
    }
    input.current?.focus();
    toast('press ⌘V to paste');
  };

  const recheck = async () => {
    setBusy('checking');
    try {
      setCheck(await invoke('check_fal_key', {}));
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  const forget = async () => {
    setBusy('saving');
    try {
      await invoke('forget_fal_key');
      setCheck(null);
      await refresh();
    } finally {
      setBusy(null);
    }
  };

  const start = () => {
    close();
    // A fresh project: the open one if it's still empty (first launch), else a new one.
    if (getState().nodes.length) void createProject('untitled');
    else {
      setState({ chatOpen: true, chatView: 'chat', subView: null });
      requestAnimationFrame(() => window.dispatchEvent(new Event('falos:focus-chat')));
    }
  };

  if (!s) return null;
  const x = s.extras;
  const extrasOk = !!x && x.motion && x.hyperframes && x.chrome && x.skills;
  const ready = required(s);
  const t = tasks;
  const keyInput = !s.falKey || editKey;

  return (
    <div className="modal" onPointerDown={close}>
      <div className="connect setup" onPointerDown={(e) => e.stopPropagation()}>
        <div className="connect-body">
          <div className="dim">set up fal-os{s.home ? <span className="faint"> · {s.home}</span> : null}</div>
          {s.error && <div className="err">setup helper failed: {s.error}</div>}

          <div className="step">
            <div className="step-head">{box(s.falKey)} 1. fal key</div>
            {s.falKey && (
              <div className="step-body">
                <span className="dim">stored in {s.keySource === 'keychain' ? 'your macOS Keychain' : `${s.home}/fal.env (0600)`}</span>{' '}
                <button className="link dim" onClick={recheck} disabled={!!busy}>[check]</button>{' '}
                <button className="link dim" onClick={() => setEditKey(!editKey)} disabled={!!busy}>[{editKey ? 'keep' : 'change'}]</button>{' '}
                <button className="link dim" onClick={forget} disabled={!!busy}>[remove]</button>
              </div>
            )}
            {keyInput && (
              <div className="step-body">
                <div className="faint">
                  create a key with the AGENT preset, copy it, paste it here. It stays on this Mac (Keychain); only the agent runtime uses it.
                </div>
                <div className="setup-row">
                  <button className="link" onClick={() => openUrl(KEYS_URL)}>[open fal.ai/dashboard/keys]</button>
                  <button className="link dim" onClick={paste} disabled={!!busy}>[paste]</button>
                </div>
                <div className="setup-row">
                  <input
                    ref={input}
                    className="setup-input"
                    type="password"
                    placeholder={s.falKey ? 'new fal key' : 'fal key'}
                    autoComplete="off"
                    spellCheck={false}
                    value={key}
                    onChange={(e) => setKey(e.target.value)}
                    // ⌘V is a gesture: check and save what was pasted right away.
                    onPaste={(e) => {
                      const v = e.clipboardData.getData('text').trim();
                      if (!v) return;
                      e.preventDefault();
                      setKey(v);
                      void saveKey(v);
                    }}
                    onKeyDown={(e) => e.key === 'Enter' && void saveKey()}
                  />
                  <button className="btn" onClick={() => void saveKey()} disabled={!!busy || !key.trim()}>{busy ?? 'save'}</button>
                </div>
              </div>
            )}
            {check && (
              <div className="step-body faint">
                models: {check.models.ok ? 'ok' : check.models.reason} · agent API: {check.agent.ok ? 'ok' : `no — ${check.agent.reason ?? ''} (generators fall back to direct model calls)`}
              </div>
            )}
            {err && <div className="step-body err">{err}</div>}
          </div>

          <div className="step">
            <div className="step-head">{box(claudeReady(s))} 2. Claude Code (runs the agents with your own Claude login)</div>
            <div className="step-body">
              {!s.claude?.path ? (
                t['claude-install']?.running ? (
                  <div className="dim">installing Claude Code…</div>
                ) : (
                  <>
                    <div className="faint">not installed. Installs Anthropic's official build into ~/.local/bin (no admin rights).</div>
                    <div className="setup-row">
                      <button className="btn" onClick={() => void run('claude-install')}>install</button>
                      {t['claude-install']?.code ? <span className="err">install failed (exit {t['claude-install'].code})</span> : null}
                    </div>
                  </>
                )
              ) : s.claude.loggedIn === false ? (
                <>
                  <div className="faint">{s.claude.path} · not signed in. Sign in with your Claude account in the browser.</div>
                  {t['claude-login']?.running ? (
                    <div className="setup-row">
                      <span className="dim">waiting for the browser sign-in…</span>
                      {t['claude-login'].url && <button className="link dim" onClick={() => openUrl(t['claude-login']!.url!)}>[open the sign-in page]</button>}
                      <button className="link dim" onClick={() => void invoke('setup_cancel', { task: 'claude-login' })?.then(pollTasks)}>[cancel]</button>
                    </div>
                  ) : (
                    <div className="setup-row">
                      <button className="btn" onClick={() => void run('claude-login')}>sign in</button>
                    </div>
                  )}
                </>
              ) : (
                <div className="dim">{s.claude.path}{s.claude.subscription ? ` · ${s.claude.subscription}` : ''} · signed in</div>
              )}
              <TaskLog t={t['claude-install'] ?? null} />
              <TaskLog t={t['claude-login'] ?? null} n={3} />
              <Fallback t={t['claude-login'] ?? null} cmd="claude auth login" />
            </div>
          </div>

          <div className="step">
            <div className="step-head">{box(!!s.ffmpeg && extrasOk)} 3. motion tools (ffmpeg, Remotion, HyperFrames, skills · ~900 MB download)</div>
            <div className="step-body">
              <div className="faint">
                ffmpeg {s.ffmpeg ? '✓' : '·'}
                {x && <> · remotion {x.motion ? '✓' : '·'} · browser {x.chrome ? '✓' : '·'} · hyperframes {x.hyperframes ? '✓' : '·'} · skills {x.skills ? '✓' : '·'}</>}
                {s.diskFreeGb != null ? ` · ${s.diskFreeGb} GB free` : ''}
              </div>
              {t.ffmpeg?.running ? (
                <div className="dim">ffmpeg: {t.ffmpeg.lines.at(-1) ?? 'starting…'}</div>
              ) : (
                !s.ffmpeg && <div className="faint">ffmpeg is required (frames, probes, QA, exports).</div>
              )}
              {s.installing && <div className="dim">installing {s.install?.step ?? ''}… (keeps going if you close this)</div>}
              {!s.installing && s.install?.error && <div className="err">{s.install.step}: {s.install.error}</div>}
              {t.ffmpeg && !t.ffmpeg.running && t.ffmpeg.code !== 0 && <TaskLog t={t.ffmpeg} n={3} />}
              {(!s.ffmpeg && !t.ffmpeg?.running) || (!extrasOk && !s.installing) ? (
                <div className="setup-row">
                  <button
                    className="btn"
                    onClick={() => {
                      if (!s.ffmpeg && !t.ffmpeg?.running) void run('ffmpeg');
                      if (!extrasOk && !s.installing) void installExtras();
                    }}
                  >
                    install
                  </button>
                </div>
              ) : null}
            </div>
          </div>

          <div className="step">
            <div className="step-head">{box(!!s.codex?.path && s.codex.signedIn)} 4. Codex (optional, for the codex driver)</div>
            <div className="step-body">
              {!s.codex?.path ? (
                t['codex-install']?.running ? (
                  <div className="dim">installing Codex…</div>
                ) : (
                  <div className="setup-row">
                    <button className="link dim" onClick={() => void run('codex-install')}>[install]</button>
                    <span className="faint">into {s.home}/npm with the bundled npm</span>
                  </div>
                )
              ) : !s.codex.signedIn ? (
                t['codex-login']?.running ? (
                  <div className="setup-row">
                    <span className="dim">waiting for the browser sign-in…</span>
                    {t['codex-login'].url && <button className="link dim" onClick={() => openUrl(t['codex-login']!.url!)}>[open the sign-in page]</button>}
                    <button className="link dim" onClick={() => void invoke('setup_cancel', { task: 'codex-login' })?.then(pollTasks)}>[cancel]</button>
                  </div>
                ) : (
                  <div className="setup-row">
                    <span className="faint">{s.codex.path}</span>
                    <button className="link dim" onClick={() => void run('codex-login')}>[sign in]</button>
                  </div>
                )
              ) : (
                <div className="faint">{s.codex.path} · signed in</div>
              )}
              <TaskLog t={t['codex-install'] ?? null} />
              <TaskLog t={t['codex-login'] ?? null} n={3} />
              <Fallback t={t['codex-login'] ?? null} cmd="codex login" />
            </div>
          </div>

          <div className="step">
            <div className="step-head">5. your own Claude / Codex (optional: they direct fal-os and your other apps)</div>
            <div className="step-body">
              <DirectorConnect />
            </div>
          </div>

          <OpenAtLogin />

          {ready ? (
            <div className="setup-row">
              <span>ready.</span>
              {launch ? (
                <button ref={finish} className="btn" onClick={start}>start →</button>
              ) : (
                <button ref={finish} className="btn" onClick={close}>done</button>
              )}
              <button className="link dim" onClick={() => void refresh()}>[recheck]</button>
              {s.installing && <span className="faint">motion tools finish in the background</span>}
            </div>
          ) : (
            <div className="setup-row">
              <button className="link dim" onClick={close}>[later]</button>
              <button className="link dim" onClick={() => void refresh()}>[recheck]</button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
