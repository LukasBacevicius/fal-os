// The Tauri desktop shell's "out" commands (apps/desktop/src-tauri/src/out.rs,
// docs/ASSETS-V1.md §7). Without the shell (web build) `isDesktop()` is false and callers use
// browser fallbacks (downloads, async clipboard).

import { toast } from './store';

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<any>;

const tauri = (): Invoke | null => {
  const f = (window as any).__TAURI_INTERNALS__?.invoke;
  return typeof f === 'function' ? f : null;
};

export const isDesktop = () => tauri() !== null;

export function invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const f = tauri();
  return f ? f(cmd, args) : Promise.reject(new Error('desktop app only'));
}

export interface AppInfo { name: string; bundle_id: string; path: string; pinned: boolean; default: boolean }

export const desktop = {
  /** Human-named APFS clone of an asset in `<project>/.out/` (absolute path). */
  materialize: (a: { sha?: string; node?: string; name?: string; project?: string | null; canvas?: string }) =>
    invoke<string>('materialize', { ...a, project: a.project ?? undefined }),
  /** Native drag of files out of the window. Call while the mouse button is down. */
  dragOut: (paths: string[], icon?: string) => invoke<void>('drag_out', { paths, icon }),
  copyFiles: (paths: string[]) => invoke<void>('copy_files', { paths }),
  copyImage: (path: string) => invoke<void>('copy_image', { path }),
  openWithList: (path: string) => invoke<AppInfo[]>('open_with_list', { path }),
  openWith: (path: string, app: AppInfo) => invoke<void>('open_with', { path, appPath: app.path, bundleId: app.bundle_id }),
  reveal: (path: string) => invoke<void>('reveal', { path }),
  /** A folder picked with the native dialog (null = cancelled). */
  chooseFolder: () => invoke<string | null>('plugin:dialog|open', { options: { directory: true, multiple: false } }),
};

const MIME: Record<string, string> = {
  png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp', heic: 'image/heic',
  tif: 'image/tiff', tiff: 'image/tiff', svg: 'image/svg+xml', mp4: 'video/mp4', mov: 'video/quicktime', webm: 'video/webm',
  m4v: 'video/x-m4v', mp3: 'audio/mpeg', wav: 'audio/wav', m4a: 'audio/mp4', aif: 'audio/aiff', aiff: 'audio/aiff',
  flac: 'audio/flac', pdf: 'application/pdf', txt: 'text/plain', md: 'text/markdown', json: 'application/json',
};

/** Protected folders already explained this session (macOS remembers the answer). */
const warnedFolders = new Set<string>();

/** Files on the system clipboard (Finder-copied files, or image data as a PNG), read natively:
 *  WKWebView's paste event doesn't reliably carry them. Empty on the web build. */
export async function nativeClipboardFiles(): Promise<File[]> {
  if (!isDesktop()) return [];
  try {
    const list = await invoke<{ path: string; name: string; folder: string | null }[]>('clipboard_files');
    // Reading a file pasted from Desktop, Documents, iCloud Drive… is when macOS asks for that
    // folder (once): say why right before.
    for (const f of new Set(list.map((l) => l.folder).filter((f): f is string => !!f))) {
      if (warnedFolders.has(f)) continue;
      warnedFolders.add(f);
      toast(`macOS may ask once whether fal-os can open files in ${f.startsWith('a ') ? f : `your ${f} folder`}: you pasted a file from there`);
    }
    const files = await Promise.all(list.map(async ({ path, name }) => {
      try {
        const buf: ArrayBuffer = await invoke('read_clip_file', { path });
        const ext = name.split('.').pop()?.toLowerCase() ?? '';
        return new File([buf], name, { type: MIME[ext] ?? '' });
      } catch {
        return null; // a folder, gone, or access denied
      }
    }));
    return files.filter((f): f is File => !!f);
  } catch {
    return [];
  }
}

