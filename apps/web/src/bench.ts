// `?bench=1`: an interaction benchmark on the open project (pan, zoom, marquee, drag of the
// selection, rapid doc edits). Results go to `window.__falosBench`, where the desktop probe
// (FALOS_PROBE) and devtools can read them. Runs on whatever is loaded; changes nothing that
// persists except the dragged selection, which it undoes.
import { getState } from './store';

type Stats = { p50: number; p90: number; p99: number; max: number; long: number; longMax: number; frames: number };

async function measure(fn: () => Promise<void>): Promise<Stats> {
  const long: number[] = [];
  let po: PerformanceObserver | null = null;
  try {
    po = new PerformanceObserver((l) => l.getEntries().forEach((e) => long.push(Math.round(e.duration))));
    po.observe({ type: 'longtask' });
  } catch {
    /* WebKit: no longtask entries */
  }
  const ft: number[] = [];
  let last = performance.now();
  let run = true;
  const tick = (t: number) => {
    ft.push(t - last);
    last = t;
    if (run) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  await fn();
  run = false;
  po?.disconnect();
  ft.shift();
  ft.sort((a, b) => a - b);
  const q = (p: number) => Math.round(ft[Math.min(ft.length - 1, Math.floor(ft.length * p))] ?? 0);
  return { p50: q(0.5), p90: q(0.9), p99: q(0.99), max: Math.round(ft[ft.length - 1] ?? 0), long: long.length, longMax: Math.max(0, ...long), frames: ft.length };
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

const postUrl = () => new URLSearchParams(location.search).get('benchpost');
function report(phase: string, data: unknown) {
  const u = postUrl();
  if (u) void fetch(u, { method: 'POST', body: JSON.stringify({ probe: 'bench-phase', phase, t: Math.round(performance.now()), data }) }).catch(() => {});
}

export async function runBench() {
  // Main-thread stall detector: how late a 100 ms timer fires.
  let lastBeat = performance.now();
  const stalls: number[] = [];
  setInterval(() => {
    const now = performance.now();
    if (now - lastBeat > 250) stalls.push(Math.round(now - lastBeat));
    lastBeat = now;
  }, 100);
  setInterval(() => report('heartbeat', { stalls: stalls.splice(0), stats: (getState() as any).stats }), 1000);
  await sleep(Number(new URLSearchParams(location.search).get('benchdelay') ?? 8000));
  const cv = document.querySelector('canvas')!;
  const r = cv.getBoundingClientRect();
  const cx = r.left + r.width / 2;
  const cy = r.top + r.height / 2;
  const wheel = (o: WheelEventInit) => cv.dispatchEvent(new WheelEvent('wheel', { clientX: cx, clientY: cy, bubbles: true, cancelable: true, ...o }));
  const pe = (type: string, x: number, y: number, b = 1) =>
    cv.dispatchEvent(new PointerEvent(type, { clientX: x, clientY: y, buttons: b, button: 0, pointerId: 1, bubbles: true, pointerType: 'mouse' }));
  const out: Record<string, unknown> = { nodes: getState().nodes.length, ua: navigator.userAgent, dpr: devicePixelRatio, w: innerWidth, h: innerHeight };
  report('start-idle', null);
  out.idle = await measure(() => sleep(1500));
  report('start-pan', null);
  out.pan = await measure(async () => {
    for (let i = 0; i < 120; i++) {
      wheel({ deltaX: i % 40 < 20 ? 30 : -30, deltaY: i % 60 < 30 ? 20 : -20 });
      await sleep(16);
    }
  });
  report('start-zoom', null);
  out.zoom = await measure(async () => {
    for (let i = 0; i < 80; i++) {
      wheel({ deltaY: i < 40 ? -10 : 10, ctrlKey: true });
      await sleep(16);
    }
  });
  report('start-marquee', null);
  out.marquee = await measure(async () => {
    pe('pointerdown', r.left + 4, r.top + 4);
    for (let i = 1; i <= 40; i++) {
      pe('pointermove', r.left + 4 + (i * (r.width - 8)) / 40, r.top + 4 + (i * (r.height - 8)) / 40);
      await sleep(16);
    }
    pe('pointerup', r.right - 4, r.bottom - 4, 0);
  });
  report('done-marquee', out.marquee);
  out.selected = getState().selection.length;
  report('start-drag', null);
  out.drag = await measure(async () => {
    pe('pointerdown', cx, cy);
    for (let i = 1; i <= 40; i++) {
      pe('pointermove', cx + i * 3, cy + i * 2);
      await sleep(16);
    }
    pe('pointerup', cx + 120, cy + 80, 0);
  });
  window.dispatchEvent(new KeyboardEvent('keydown', { key: 'z', code: 'KeyZ', metaKey: true, bubbles: true }));
  (window as any).__falosBench = out;
  const post = new URLSearchParams(location.search).get('benchpost');
  if (post) void fetch(post, { method: 'POST', body: JSON.stringify({ probe: 'bench', ...out }) }).catch(() => {});
  console.log('[bench]', JSON.stringify(out));
}
