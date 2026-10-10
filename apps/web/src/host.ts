// Host discovery: where the local fal-os host (crates/host) listens and its token.
//
// 1. `?host=127.0.0.1:7317&token=…` in the page URL (explicit; wins).
// 2. Tauri shell: the `host_info` command reads ~/.fal-os/host.json (FALOS_HOME honored).
// 3. Vite dev server: GET /__falos/host serves the same file (dev only, loopback only).
// No host → the canvas stays local-only (IndexedDB).

export interface HostInfo {
  /** `127.0.0.1:<port>` */
  addr: string;
  token: string;
  project?: string;
}

function fromQuery(): HostInfo | null {
  const q = new URLSearchParams(location.search);
  const addr = q.get('host');
  const token = q.get('token');
  if (q.has('local')) return null;
  return addr && token ? { addr, token } : null;
}

async function fromTauri(): Promise<HostInfo | null> {
  const invoke = (window as any).__TAURI_INTERNALS__?.invoke;
  if (typeof invoke !== 'function') return null;
  try {
    const h = await invoke('host_info');
    return h?.port && h?.token ? { addr: `127.0.0.1:${h.port}`, token: h.token, project: h.project } : null;
  } catch {
    return null;
  }
}

async function fromDevServer(): Promise<HostInfo | null> {
  if (!import.meta.env.DEV) return null;
  try {
    const r = await fetch('/__falos/host', { cache: 'no-store' });
    if (!r.ok) return null;
    const h = await r.json();
    return h?.port && h?.token ? { addr: `127.0.0.1:${h.port}`, token: h.token, project: h.project } : null;
  } catch {
    return null;
  }
}

/** Finds the host. Re-run on reconnect: a restarted host may have a new port. */
export async function discoverHost(): Promise<HostInfo | null> {
  if (new URLSearchParams(location.search).has('local')) return null;
  return fromQuery() ?? (await fromTauri()) ?? (await fromDevServer());
}

/** Whether the page was configured with an explicit host (no rediscovery). */
export const explicitHost = () => fromQuery() !== null;
