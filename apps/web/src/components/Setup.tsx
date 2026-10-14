import { useCallback, useEffect, useRef, useState } from 'react';
import { getState, setState, toast } from '../store';
import { createProject } from '../projects';
import { DirectorConnect } from './DirectorConnect';

// First-run setup for the packaged desktop app (apps/desktop, Tauri commands in
// src-tauri/src/sidecar.rs). Two steps: paste a fal key, sign in to Claude. Everything else
// (ffmpeg, the motion tools) starts by itself in the background; Codex, connecting your own
// Claude / Codex and open at login sit collapsed under [more]. Shown on launch until the key and
// the Claude sign-in are there; later from the palette ("setup…"). Outside the packaged app it
// never opens.
//
// No macOS permission prompts here (docs/PACKAGING.md §2.5.1): the key is stored in a 0600 file
// (no Keychain dialog), [paste] reads the clipboard natively (no WebKit "Paste" bubble). The two
// prompts that can remain are explained on the line right before they appear: moving a key an
// older build kept in the Keychain, and "open at login" (macOS's "Background Items Added").

type Status = {
  packaged: boolean;
  error?: string;
  claude?: { path: string | null; loggedIn: boolean | null; subscription?: string | null };
  codex?: { path: string | null; signedIn: boolean };
  ffmpeg?: boolean;
  ffmpegPath?: string | null;
  falKey?: boolean;
  keySource?: 'file' | 'none';
  /** An older build left the key in the Keychain (found without reading it). */
  keychainKey?: boolean;
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
/** The motion tools are ~0.9 GB; below this, don't start them unasked. */
const AUTO_MIN_FREE_GB = 3;

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
const required = (s: Status) => !!s.falKey && claudeReady(s);
const extrasDone = (s: Status) => !!s.extras && s.extras.motion && s.extras.hyperframes && s.extras.chrome && s.extras.skills;

/** Start the downloads nobody needs to click (ffmpeg, motion tools), once per launch. */
let autoStarted = false;
function autoInstall(s: Status) {
  if (autoStarted || !s.packaged || s.error) return;
  if (s.diskFreeGb != null && s.diskFreeGb < AUTO_MIN_FREE_GB) return;
  autoStarted = true;
  if (!s.ffmpeg) invoke('setup_run', { task: 'ffmpeg' })?.catch(() => {});
  if (!extrasDone(s) && !s.installing) invoke('install_extras')?.catch(() => {});
}

/** Opened at launch (first run) rather than from the palette: "start" then opens a project. */
let firstRun = false;

/** At launch in the packaged app: start the background downloads, and open the setup screen
 *  while the key or the Claude sign-in is missing. */
export async function maybeOpenSetup() {
  const s = await setupStatus();
  if (!s?.packaged) return;
  autoInstall(s);
  if (!required(s)) {
    firstRun = true;
    setState({ overlay: 'setup' });
  }
}

/** Clipboard text: natively in the desktop app (no WebKit "Paste" permission bubble). */
async function clipboardText(): Promise<string> {
  const native = invoke('clipboard_text');
  if (native) return String((await native) ?? '');
  return navigator.clipboard.readText();
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
    <div className="step">
      <div className="setup-row">
        <button className="link dim" onClick={toggle}>{box(on)} open at login</button>
        <span className="faint">starts hidden in the menu bar, so agents keep running</span>
      </div>
      {!on && <div className="step-body faint">turning it on makes macOS show a "Background Items Added" notice; that's this.</div>}
    </div>
  );
}

/** A sign-in that failed (not cancelled): the Terminal command as a last resort. */
function Fallback({ t, cmd }: { t: Task; cmd: string }) {
  if (!t || t.running || !t.code || t.code < 0) return null;
  return <div className="faint">didn't work? try again, or run `{cmd}` in Terminal.</div>;
}

export function Setup() {
  const [s, setS] = useState<Status | null>(null);
  const [tasks, setTasks] = useState<Partial<Record<TaskName, Task>>>({});
  const [key, setKey] = useState('');
  const [editKey, setEditKey] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [check, setCheck] = useState<KeyCheck | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [more, setMore] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  // [sign in] with Claude Code missing installs it first, then signs in.
  const loginAfterInstall = useRef(false);
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
    if (v) autoInstall(v);
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
  // While the downloads run, and while something required is missing (the user may sign in to
  // Claude in the browser), poll the status.
  const pending = !!s && (!required(s) || !!s.installing || !s.ffmpeg);
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

  const run = useCallback(async (task: TaskName) => {
    setErr(null);
    try {
      await invoke('setup_run', { task });
    } catch (e) {
      setErr(String(e));
    }
    await pollTasks();
  }, [pollTasks]);

  // Claude Code just got installed by [sign in]: go on to the browser sign-in.
  const install = tasks['claude-install'];
  useEffect(() => {
    if (!loginAfterInstall.current || !s || install?.running) return;
    if (install && install.code !== 0) {
      loginAfterInstall.current = false;
      return;
    }
    if (s.claude?.path) {
      loginAfterInstall.current = false;
      if (s.claude.loggedIn === false && !tasks['claude-login']?.running) void run('claude-login');
    }
  }, [s, install, tasks, run]);

  const signIn = () => {
    if (s?.claude?.path) return void run('claude-login');
    loginAfterInstall.current = true;
    void run('claude-install');
  };

  const installExtras = async () => {
    try {
      await invoke('install_extras');
    } catch (e) {
      setErr(String(e));
    }
    await refresh();
  };

  const saved = async (where: string) => {
    setKey('');
    setEditKey(false);
    toast(where === 'keychain' ? 'fal key saved' : `fal key saved in ${s?.home ?? '~/.fal-os'}/fal.env`);
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
      await saved(await invoke('save_fal_key', { key: k }));
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  const migrate = async () => {
    setErr(null);
    setBusy('moving');
    try {
      await saved(await invoke('migrate_fal_key'));
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  // Reads the clipboard only on this click (a user gesture), never on its own.
  const paste = async () => {
    try {
      const t = (await clipboardText()).trim();
      if (t) {
        setKey(t);
        void saveKey(t);
        return;
      }
    } catch {
      /* not allowed: fall through to ⌘V */
    }
    input.current?.focus();
    toast('copy the key first, or press ⌘V here');
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
  const extrasOk = extrasDone(s);
  const ready = required(s);
  const t = tasks;
  const keyInput = !s.falKey || editKey;
  const toolsOk = !!s.ffmpeg && extrasOk;
  const toolsFailed = (!!t.ffmpeg && !t.ffmpeg.running && t.ffmpeg.code !== 0) || (!s.installing && !!s.install?.error);
  const login = t['claude-login'];

  // One line on the background downloads: what runs now, or what failed.
  const toolsLine = toolsOk
    ? null
    : t.ffmpeg?.running
      ? `ffmpeg: ${t.ffmpeg.lines.at(-1) ?? 'starting…'}`
      : s.installing
        ? `motion tools: ${s.install?.step ?? 'installing'}…`
        : toolsFailed
          ? `motion tools didn't install${s.install?.error ? ` (${s.install.step}: ${s.install.error})` : ''}`
          : 'motion tools not installed yet';

  return (
    <div className="modal" onPointerDown={close}>
      <div className="connect setup" onPointerDown={(e) => e.stopPropagation()}>
        <div className="connect-body">
          <div className="dim">set up fal OS</div>
          {s.error && <div className="err">setup helper failed: {s.error}</div>}

          <div className="step">
            <div className="step-head">{box(s.falKey)} 1. paste your fal key</div>
            {s.falKey && (
              <div className="step-body">
                <span className="dim">saved on this Mac ({s.home}/fal.env, only you can read it)</span>{' '}
                <button className="link dim" onClick={recheck} disabled={!!busy}>[check]</button>{' '}
                <button className="link dim" onClick={() => setEditKey(!editKey)} disabled={!!busy}>[{editKey ? 'keep' : 'change'}]</button>{' '}
                <button className="link dim" onClick={forget} disabled={!!busy}>[remove]</button>
              </div>
            )}
            {!s.falKey && s.keychainKey && (
              <div className="step-body">
                <div className="faint">an earlier fal OS kept your key in the Keychain. macOS asks once to let fal OS read it: click Allow.</div>
                <div className="setup-row">
                  <button className="btn" onClick={() => void migrate()} disabled={!!busy}>{busy === 'moving' ? 'moving…' : 'use that key'}</button>
                  <span className="faint">or paste a key below</span>
                </div>
              </div>
            )}
            {keyInput && (
              <div className="step-body">
                <div className="faint">create a key with the AGENT preset, copy it, then [paste].</div>
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
            <div className="step-head">{box(claudeReady(s))} 2. sign in to Claude</div>
            <div className="step-body">
              {claudeReady(s) ? (
                <div className="dim">{s.claude!.path}{s.claude!.subscription ? ` · ${s.claude!.subscription}` : ''} · signed in</div>
              ) : t['claude-install']?.running ? (
                <div className="dim">installing Claude Code (Anthropic's official build, into ~/.local/bin)…</div>
              ) : login?.running ? (
                <div className="setup-row">
                  <span className="dim">finish the sign-in in your browser…</span>
                  {login.url && <button className="link dim" onClick={() => openUrl(login.url!)}>[open the sign-in page]</button>}
                  <button className="link dim" onClick={() => void invoke('setup_cancel', { task: 'claude-login' })?.then(pollTasks)}>[cancel]</button>
                </div>
              ) : (
                <>
                  <div className="faint">the agents run with your own Claude account.{s.claude?.path ? '' : ' Installs Claude Code first (no admin rights).'}</div>
                  <div className="setup-row">
                    <button className="btn" onClick={signIn}>sign in</button>
                    {t['claude-install']?.code ? <span className="err">install failed (exit {t['claude-install'].code})</span> : null}
                  </div>
                </>
              )}
              <TaskLog t={t['claude-install'] ?? null} />
              <TaskLog t={login ?? null} n={3} />
              <Fallback t={login ?? null} cmd="claude auth login" />
            </div>
          </div>

          {ready ? (
            <div className="setup-row">
              <span>ready.</span>
              {launch ? (
                <button ref={finish} className="btn" onClick={start}>start →</button>
              ) : (
                <button ref={finish} className="btn" onClick={close}>done</button>
              )}
            </div>
          ) : (
            <div className="setup-row">
              <button className="link dim" onClick={close}>[later]</button>
            </div>
          )}

          <div className="step">
            {toolsLine && (
              <div className="setup-row faint">
                <span>{toolsLine}{toolsFailed ? '' : ' (in the background)'}</span>
                {toolsFailed && !more && <button className="link dim" onClick={() => setMore(true)}>[retry]</button>}
              </div>
            )}
            <div className="setup-row">
              <button className="link dim nowrap" onClick={() => setMore(!more)}>[{more ? '−' : '+'}] more</button>
              <span className="faint">optional: Codex, your own Claude / Codex, open at login</span>
            </div>
            {more && (
              <div className="setup-more">
                <div className="step">
                  <div className="step-head">{box(toolsOk)} motion tools (ffmpeg, Remotion, HyperFrames, skills · ~900 MB, downloads by itself)</div>
                  <div className="step-body">
                    <div className="faint">
                      ffmpeg {s.ffmpeg ? '✓' : '·'}
                      {x && <> · remotion {x.motion ? '✓' : '·'} · browser {x.chrome ? '✓' : '·'} · hyperframes {x.hyperframes ? '✓' : '·'} · skills {x.skills ? '✓' : '·'}</>}
                      {s.diskFreeGb != null ? ` · ${s.diskFreeGb} GB free` : ''}
                    </div>
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
                          {toolsFailed ? 'retry' : 'install'}
                        </button>
                      </div>
                    ) : null}
                  </div>
                </div>

                <div className="step">
                  <div className="step-head">{box(!!s.codex?.path && s.codex.signedIn)} Codex (for the codex driver)</div>
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
                          <span className="dim">finish the sign-in in your browser…</span>
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
                  <div className="step-head">your own Claude / Codex (they direct fal OS and your other apps)</div>
                  <div className="step-body">
                    <DirectorConnect />
                  </div>
                </div>

                <OpenAtLogin />
                <div className="faint">{s.home}</div>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
