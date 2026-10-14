import { useEffect, useRef } from 'react';

export interface MeasureLabel { x: number; y: number; text: string; vertical: boolean }

const listeners = new Set<(l: MeasureLabel[]) => void>();

/** The engine's `measure` event: ⌥ distance labels (CSS px on the canvas). */
export function setMeasure(labels: MeasureLabel[]) {
  listeners.forEach((f) => f(labels));
}

/** ⌥ distances: tiny mono numbers on the engine's distance lines. Written to the DOM directly
 *  (no React render per change); the lines themselves are drawn by the engine. */
export function MeasureLabels() {
  const el = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const draw = (labels: MeasureLabel[]) => {
      const root = el.current;
      if (!root) return;
      root.replaceChildren(
        ...labels.map((l) => {
          const s = document.createElement('span');
          s.className = 'measure-label';
          s.textContent = l.text;
          // Beside a vertical line, above a horizontal one.
          s.style.transform = l.vertical ? `translate(${l.x + 4}px, ${l.y - 7}px)` : `translate(calc(${l.x}px - 50%), ${l.y - 16}px)`;
          return s;
        }),
      );
    };
    listeners.add(draw);
    return () => void listeners.delete(draw);
  }, []);
  return <div className="measure-layer" ref={el} />;
}
