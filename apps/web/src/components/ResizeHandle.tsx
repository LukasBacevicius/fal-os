// Resizable side panels. Widths live in CSS variables on <html> (`--layers-w`,
// `--libpanel-w`, `--chat-w`): a drag writes the variable once per frame and never renders
// React; the stage's ResizeObserver resizes the canvas (no flash: see Stage.tsx). The width
// is saved on release; a double-click resets it.

export type Panel = 'layers' | 'library' | 'chat';

interface Spec { cssVar: string; key: string; def: number; min: number; max: number }

export const PANELS: Record<Panel, Spec> = {
  layers: { cssVar: '--layers-w', key: 'falos.layersWidth', def: 232, min: 180, max: 520 },
  library: { cssVar: '--libpanel-w', key: 'falos.libraryWidth', def: 320, min: 200, max: 720 },
  chat: { cssVar: '--chat-w', key: 'falos.chatWidth', def: 360, min: 280, max: 760 },
};

/** The canvas keeps at least this much room. */
const MIN_STAGE = 240;

function clamp(p: Spec, w: number): number {
  const room = Math.max(p.min, innerWidth - MIN_STAGE - 240);
  return Math.round(Math.max(p.min, Math.min(p.max, room, w)));
}

function set(p: Spec, w: number) {
  document.documentElement.style.setProperty(p.cssVar, `${w}px`);
}

/** Applies the saved widths before the first paint. */
export function initPanelWidths() {
  for (const p of Object.values(PANELS)) {
    let w = p.def;
    try {
      const v = Number(localStorage.getItem(p.key));
      if (Number.isFinite(v) && v > 0) w = v;
    } catch {
      /* private mode */
    }
    set(p, clamp(p, w));
  }
}

/**
 * A thin drag handle on a panel's inner edge (`edge`: the side of the panel it sits on).
 * col-resize cursor, a 1px line lit on hover and while dragging, double-click resets.
 */
export function ResizeHandle({ panel, edge }: { panel: Panel; edge: 'left' | 'right' }) {
  const p = PANELS[panel];
  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const handle = e.currentTarget;
    const box = handle.parentElement!.getBoundingClientRect();
    const x0 = e.clientX;
    const w0 = box.width;
    let next = w0;
    let raf = 0;
    handle.setPointerCapture(e.pointerId);
    handle.classList.add('on');
    document.body.classList.add('col-resizing');
    const move = (ev: PointerEvent) => {
      const dx = ev.clientX - x0;
      next = clamp(p, edge === 'right' ? w0 + dx : w0 - dx);
      if (!raf) raf = requestAnimationFrame(() => {
        raf = 0;
        set(p, next);
      });
    };
    const up = () => {
      cancelAnimationFrame(raf);
      set(p, next);
      handle.classList.remove('on');
      document.body.classList.remove('col-resizing');
      handle.removeEventListener('pointermove', move);
      handle.removeEventListener('pointerup', up);
      handle.removeEventListener('pointercancel', up);
      try {
        localStorage.setItem(p.key, String(next));
      } catch {
        /* private mode */
      }
    };
    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', up);
    handle.addEventListener('pointercancel', up);
  };
  const reset = () => {
    set(p, clamp(p, p.def));
    try {
      localStorage.removeItem(p.key);
    } catch {
      /* private mode */
    }
  };
  return (
    <div
      className={`resize-handle ${edge}`}
      role="separator"
      aria-orientation="vertical"
      title="drag to resize · double-click resets"
      onPointerDown={onPointerDown}
      onDoubleClick={reset}
    />
  );
}
