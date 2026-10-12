// Theme: dark by default; `?theme=light` (remembered) switches to the light variant of the
// same CSS variables, and the canvas follows via `CanvasApp.setTheme`.

export type Theme = 'dark' | 'light';

export function theme(): Theme {
  const q = new URLSearchParams(location.search).get('theme');
  try {
    if (q === 'light' || q === 'dark') localStorage.setItem('falos.theme', q);
    return localStorage.getItem('falos.theme') === 'light' ? 'light' : 'dark';
  } catch {
    return q === 'light' ? 'light' : 'dark';
  }
}

export function applyTheme() {
  document.documentElement.dataset.theme = theme();
}

/** Switches the theme at runtime (remembered); the canvas follows. */
export function setTheme(t: Theme, app?: { setTheme(t: string): void } | null) {
  try {
    localStorage.setItem('falos.theme', t);
  } catch {
    /* private mode */
  }
  const u = new URL(location.href);
  if (u.searchParams.has('theme')) {
    u.searchParams.delete('theme');
    history.replaceState(null, '', u);
  }
  document.documentElement.dataset.theme = t;
  app?.setTheme(t);
}
