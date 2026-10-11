import { useCallback, useEffect, useRef, useState } from 'react';
import { setState, toast } from '../store';

// First-run setup for the packaged desktop app (apps/desktop, Tauri commands in
// src-tauri/src/sidecar.rs). Shown on launch until a fal key is stored and Claude Code is
// signed in; later from the palette ("setup…"). Outside the packaged app it never opens.

type Status = {
  packaged: boolean;
  error?: string;
  claude?: { path: string | null; loggedIn: boolean | null; subscription?: string | null };
  codex?: { path: string | null; signedIn: boolean };
  ffmpeg?: boolean;
  falKey?: boolean;
  keySource?: 'keychain' | 'file' | 'none';
  extras?: { motion: boolean; hyperframes: boolean; chrome: boolean; skills: boolean };
  install?: { running?: boolean; step?: string; error?: string; done?: boolean } | null;
  installing?: boolean;
  diskFreeGb?: number | null;
  home?: string;
};

type KeyCheck = { ok: boolean; agent: { ok: boolean; reason?: string }; models: { ok: boolean; reason?: string } };

const invoke = (cmd: string, args?: Record<string, unknown>): Promise<any> | null => {
  const f = (window as any).__TAURI_INTERNALS__?.invoke;
  return typeof f === 'function' ? f(cmd, args) : null;
};

export async function setupStatus(): Promise<Status | null> {
  try {
    return (await invoke('setup_status')) ?? null;
  } catch {
    return null;
  }
}

/** Open the setup screen at launch when the packaged app still needs a fal key or Claude. */
export async function maybeOpenSetup() {
  const s = await setupStatus();
  if (s?.packaged && (!s.falKey || s.claude?.loggedIn === false || !s.claude?.path)) setState({ overlay: 'setup' });
}

const box = (ok: boolean | null | undefined) => (ok ? '[x]' : '[ ]');

function Copy({ text }: { text: string }) {
  return (
    <div className="snippet">
      <pre>{text}</pre>
      <button className="link dim" onClick={() => navigator.clipboard?.writeText(text).then(() => toast('copied'))}>[copy]</button>
    </div>
  );
}

export function Setup() {
  const [s, setS] = useState<Status | null>(null);
  const [key, setKey] = useState('');
  const [busy, setBusy] = useState<string | null>(null);
  const [check, setCheck] = useState<KeyCheck | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const close = () => setState({ overlay: null });

  const refresh = useCallback(async () => setS(await setupStatus()), []);
  useEffect(() => {
    void refresh();
  }, [refresh]);
  // Poll while the motion tools install.
  useEffect(() => {
    if (!s?.installing) return;
    const t = window.setInterval(() => void refresh(), 1500);
    return () => window.clearInterval(t);
  }, [s?.installing, refresh]);
  useEffect(() => input.current?.focus(), [s?.falKey]);

  const saveKey = async () => {
    const k = key.trim();
    if (!k) return;
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
      toast(where === 'keychain' ? 'fal key saved in your Keychain' : 'fal key saved (~/.fal-os/fal.env)');
      await refresh();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
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

  const install = async () => {
    try {
      await invoke('install_extras');
    } catch (e) {
      setErr(String(e));
    }
    await refresh();
  };

  if (!s) return null;
  const claudeOk = !!s.claude?.path && s.claude.loggedIn !== false;
  const x = s.extras;
  const extrasOk = !!x && x.motion && x.hyperframes && x.chrome && x.skills;
  const ready = !!s.falKey && claudeOk;

  return (
    <div className="modal" onPointerDown={close}>
      <div className="connect setup" onPointerDown={(e) => e.stopPropagation()} onKeyDown={(e) => e.key === 'Escape' && close()}>
        <div className="connect-body">
          <div className="dim">set up fal-os{s.home ? <span className="faint"> · {s.home}</span> : null}</div>
          {s.error && <div className="err">setup helper failed: {s.error}</div>}

          <div className="step">
            <div className="step-head">{box(s.falKey)} 1. fal key</div>
            {s.falKey ? (
              <div className="step-body">
                <span className="dim">stored in {s.keySource === 'keychain' ? 'your macOS Keychain' : `${s.home}/fal.env (0600)`}</span>{' '}
                <button className="link dim" onClick={recheck} disabled={!!busy}>[check]</button>{' '}
                <button className="link dim" onClick={forget} disabled={!!busy}>[remove]</button>
              </div>
            ) : (
              <div className="step-body">
                <div className="faint">create a key with the AGENT preset at fal.ai/dashboard/keys, paste it here. It stays on this Mac (Keychain) and only the agent runtime uses it.</div>
                <div className="setup-row">
                  <input
                    ref={input}
                    className="setup-input"
                    type="password"
                    placeholder="fal key"
                    autoComplete="off"
                    spellCheck={false}
                    value={key}
                    onChange={(e) => setKey(e.target.value)}
                    onKeyDown={(e) => e.key === 'Enter' && void saveKey()}
                  />
                  <button className="btn" onClick={saveKey} disabled={!!busy || !key.trim()}>{busy ?? 'save'}</button>
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
            <div className="step-head">{box(claudeOk)} 2. Claude Code (runs the agents with your own Claude login)</div>
            {!s.claude?.path ? (
              <div className="step-body">
                <div className="faint">install Claude Code, sign in, then reopen this screen:</div>
                <Copy text={'curl -fsSL https://claude.ai/install.sh | bash\nclaude   # sign in once'} />
              </div>
            ) : s.claude.loggedIn === false ? (
              <div className="step-body">
                <div className="faint">found {s.claude.path}, not signed in:</div>
                <Copy text="claude auth login" />
              </div>
            ) : (
              <div className="step-body dim">{s.claude.path}{s.claude.subscription ? ` · ${s.claude.subscription}` : ''}</div>
            )}
          </div>

          <div className="step">
            <div className="step-head">{box(extrasOk)} 3. motion tools (Remotion, HyperFrames, skills · ~800 MB download)</div>
            <div className="step-body">
              {x && (
                <div className="faint">
                  remotion {x.motion ? '✓' : '·'} · browser {x.chrome ? '✓' : '·'} · hyperframes {x.hyperframes ? '✓' : '·'} · skills {x.skills ? '✓' : '·'}
                  {s.diskFreeGb != null ? ` · ${s.diskFreeGb} GB free` : ''}
                </div>
              )}
              {s.installing ? (
                <div className="dim">installing {s.install?.step ?? ''}…</div>
              ) : extrasOk ? null : (
                <button className="btn" onClick={install}>install</button>
              )}
              {!s.installing && s.install?.error && <div className="err">{s.install.step}: {s.install.error}</div>}
              {!s.ffmpeg && (
                <>
                  <div className="faint">ffmpeg is needed for frames, QA and HyperFrames:</div>
                  <Copy text="brew install ffmpeg" />
                </>
              )}
            </div>
          </div>

          <div className="step">
            <div className="step-head">{box(!!s.codex?.path && s.codex.signedIn)} 4. Codex (optional)</div>
            <div className="step-body faint">
              {s.codex?.path ? (s.codex.signedIn ? `${s.codex.path} · signed in` : 'run `codex login`') : 'only if you want the codex driver: `npm i -g @openai/codex && codex login`'}
            </div>
          </div>

          <div className="setup-row">
            <button className="btn" onClick={close}>{ready ? 'done' : 'later'}</button>
            <button className="link dim" onClick={() => void refresh()}>[recheck]</button>
          </div>
        </div>
      </div>
    </div>
  );
}
