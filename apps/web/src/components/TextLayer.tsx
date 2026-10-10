import { useEffect, useLayoutEffect, useMemo, useRef } from 'react';
import { getState, setState, useStore } from '../store';
import { inView, worldRects } from '../geometry';
import type { NodeView } from '../types';

/** Font size in world units (CSS px at 100%). */
export const NOTE_FONT = 12.5;
/** Below this on-screen font size the text is not laid out (the box stays). */
const MIN_FONT_PX = 3.5;

const isText = (n: NodeView) => n.kind === 'note' || n.kind === 'text';

/** Starts in-place editing of a note/text node. */
export function editNode(id: string) {
  const n = getState().nodes.find((x) => x.id === id);
  if (n && isText(n)) setState({ editing: id });
}

/** Creates an empty note at a canvas point (CSS px) and starts editing it. */
export function createNote(screen: [number, number]) {
  const { app } = getState();
  if (!app) return;
  const [x, y] = app.screenToWorld(screen[0], screen[1]);
  const r = app.apply({ op: 'create', kind: 'note', props: { x: Math.round(x), y: Math.round(y), w: 240, h: 120 }, text: '' }) as { created: string[] };
  const id = r.created?.[0];
  if (id) {
    app.setSelection([id]);
    setState({ editing: id });
  }
}

/** Common prefix length (for keeping the caret in place across remote edits). */
function prefix(a: string, b: string) {
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) i++;
  return i;
}

function Editor({ node }: { node: NodeView }) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const text = node.text ?? '';

  useEffect(() => {
    const el = ref.current!;
    el.value = text;
    el.focus();
    el.setSelectionRange(text.length, text.length);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [node.id]);

  // Remote edits (agents, other clients) arrive merged in the doc: show them and keep the
  // caret where it was relative to the surrounding text.
  useLayoutEffect(() => {
    const el = ref.current!;
    if (el.value === text) return;
    const old = el.value;
    const [s, e] = [el.selectionStart, el.selectionEnd];
    const p = prefix(old, text);
    const d = text.length - old.length;
    el.value = text;
    el.setSelectionRange(s > p ? s + d : s, e > p ? e + d : e);
  }, [text]);

  const commit = () => {
    const { app } = getState();
    const v = ref.current!.value;
    // A minimal diff into the node's LoroText (Command::SetText): concurrent edits merge.
    if (app && v !== (getState().nodes.find((n) => n.id === node.id)?.text ?? '')) app.apply({ op: 'text', id: node.id, text: v });
  };

  return (
    <textarea
      ref={ref}
      className="note-edit"
      spellCheck={false}
      onInput={commit}
      onBlur={() => {
        commit();
        setState({ editing: null });
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') {
          e.preventDefault();
          ref.current!.blur();
        }
        e.stopPropagation();
      }}
      onPointerDown={(e) => e.stopPropagation()}
    />
  );
}

/**
 * Note and text nodes as crisp DOM text over the engine's boxes. One container carries the
 * camera transform; only nodes on screen and big enough to read get elements. Double-click
 * or Enter edits in place; every edit is a minimal LoroText diff, so it syncs live.
 */
export function TextLayer() {
  const nodes = useStore((s) => s.nodes);
  const camera = useStore((s) => s.camera);
  const editing = useStore((s) => s.editing);
  const rects = useMemo(() => worldRects(nodes), [nodes]);
  const texts = useMemo(() => nodes.filter(isText), [nodes]);
  const readable = camera.zoom * NOTE_FONT >= MIN_FONT_PX;
  const shown = texts.filter((n) => {
    if (n.id === editing) return true;
    const r = rects.get(n.id);
    return !!r && readable && r[3] * camera.zoom >= 8 && inView(r, camera);
  });
  const z = camera.zoom;
  return (
    <div className="text-layer" style={{ transform: `translate(${-camera.x * z}px, ${-camera.y * z}px) scale(${z})` }}>
      {shown.map((n) => {
        const r = rects.get(n.id)!;
        const style = { left: r[0], top: r[1], width: r[2], height: r[3], fontSize: NOTE_FONT };
        return (
          <div key={n.id} className="note-text" style={style}>
            {n.id === editing ? <Editor node={n} /> : n.text || (n.kind === 'text' ? <span className="faint">{n.name}</span> : null)}
          </div>
        );
      })}
    </div>
  );
}
