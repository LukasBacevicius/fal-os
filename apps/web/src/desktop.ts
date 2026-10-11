// The Tauri desktop shell's "out" commands (apps/desktop/src-tauri/src/out.rs,
// docs/ASSETS-V1.md §7). Without the shell (web build) `isDesktop()` is false and callers use
// browser fallbacks (downloads, async clipboard).

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
