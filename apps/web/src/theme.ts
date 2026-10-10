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
