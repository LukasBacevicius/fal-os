import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { getState, setState, useStore } from '../store';
import { worldRects } from '../geometry';
import { addChips, chatTitle, isExternal, type Chip } from '../chat';
import { sendTo } from '../chatActions';
import { MOD } from '../keys';
import { Chips } from './Chips';

/** Drafts per selection (sorted ids): text and the chips removed from it. */
const drafts = new Map<string, { text: string; removed: string[] }>();

export function openPrompt() {
  if (getState().selection.length) setState({ promptOpen: true });
}

/** Screen box of the current selection (CSS px), or null. */
export function useSelectionBox() {
  const app = useStore((s) => s.app);
  const nodes = useStore((s) => s.nodes);
  const selection = useStore((s) => s.selection);
  useStore((s) => s.camera);
  const rects = useMemo(() => worldRects(nodes), [nodes]);
  if (!app || !selection.length) return null;
  const sel = selection.map((id) => rects.get(id)).filter((r) => !!r) as [number, number, number, number][];
  if (!sel.length) return null;
  const x0 = Math.min(...sel.map((r) => r[0]));
  const y0 = Math.min(...sel.map((r) => r[1]));
  const x1 = Math.max(...sel.map((r) => r[0] + r[2]));
  const y1 = Math.max(...sel.map((r) => r[1] + r[3]));
  const [l, t] = app.worldToScreen(x0, y0);
  const [r, b] = app.worldToScreen(x1, y1);
  return { left: l, top: t, right: r, bottom: b };
}

/** The ✦ inline prompt (UX-V1 §1): follows the selection, pre-filled with it as chips.
 *  ⏎ starts a new chat, ⌘⏎ sends to the current chat, ⇥ moves the draft into the panel,
 *  esc closes (the draft is kept for this selection). */
export function SelectionPrompt() {
  const open = useStore((s) => s.promptOpen);
  const selection = useStore((s) => s.selection);
  const camera = useStore((s) => s.camera);
  const chat = useStore((s) => s.chat);
  const box = useSelectionBox();
  const key = [...selection].sort().join(',');
  const [text, setText] = useState('');
  const [removed, setRemoved] = useState<string[]>([]);
  const input = useRef<HTMLTextAreaElement>(null);
  const el = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState<[number, number]>([340, 90]);

  // Load this selection's draft; keep the previous one.
  useEffect(() => {
    const d = drafts.get(key);
    setText(d?.text ?? '');
    setRemoved(d?.removed ?? []);
  }, [key]);
  useEffect(() => {
    if (key) drafts.set(key, { text, removed });
  }, [key, text, removed]);

  useEffect(() => {
    if (open && !selection.length) setState({ promptOpen: false });
  }, [open, selection.length]);

  useEffect(() => {
    if (open) requestAnimationFrame(() => input.current?.focus());
  }, [open, key]);

  useLayoutEffect(() => {
    if (!el.current) return;
    const r = el.current.getBoundingClientRect();
    if (r.width !== size[0] || r.height !== size[1]) setSize([r.width, r.height]);
  });

  useLayoutEffect(() => {
    const t = input.current;
    if (!t) return;
    t.style.height = '0px';
    t.style.height = `${Math.min(t.scrollHeight, 6 * 18)}px`;
  }, [text, open]);

  if (!open || !box) return null;
  const chips: Chip[] = selection.filter((id) => !removed.includes(id)).map((node) => ({ kind: 'node', node }));
  const current = chat.active && !isExternal(chat.active) ? chat.active : null;

  // Right of the selection's top-right corner; flips left / clamps inside the canvas.
  let left = box.right + 10;
  if (left + size[0] > camera.width - 8) left = Math.max(8, box.left - size[0] - 10);
  if (left < 8 || box.right - box.left > camera.width * 0.6) left = Math.min(Math.max(8, box.right - size[0]), camera.width - size[0] - 8);
  left = Math.max(8, left);
  const top = Math.max(8, Math.min(box.top, camera.height - size[1] - 8));
  const width = Math.min(340, camera.width - 16);

  const close = () => setState({ promptOpen: false });
  const done = () => {
    drafts.delete(key);
    setText('');
    setRemoved([]);
    close();
  };
  const send = (to: string | null) => {
    const v = text.trim();
    if (!v) return;
    if (sendTo(to, v, chips)) done();
  };
  const toPanel = () => {
    const target = current ?? '';
    setState((s) => ({
      chatOpen: true,
      chatView: 'chat',
      chat: { ...s.chat, chips: addChips(s.chat.chips, chips) },
      drafts: text ? { ...s.drafts, [target]: [s.drafts[target], text].filter(Boolean).join(' ') } : s.drafts,
    }));
    done();
    requestAnimationFrame(() => window.dispatchEvent(new Event('falos:focus-chat')));
  };

  return (
    <div className="sel-prompt" ref={el} style={{ transform: `translate(${left}px, ${top}px)`, width }} onPointerDown={(e) => e.stopPropagation()}>
      <Chips chips={chips} onRemove={(i) => setRemoved((r) => [...r, (chips[i] as { node: string }).node])} />
      <div className="prompt">
        <span className="dim">✦</span>
        <textarea
          ref={input}
          rows={1}
          value={text}
          placeholder="ask about the selection…"
          spellCheck={false}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            const mod = e.metaKey || e.ctrlKey;
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              send(mod ? current : null);
            } else if (e.key === 'Tab' && !e.shiftKey) {
              e.preventDefault();
              toPanel();
            } else if (e.key === 'Escape') {
              e.preventDefault();
              close();
            }
            e.stopPropagation();
          }}
        />
      </div>
      <div className="sel-hint faint">
        ⏎ new chat{current ? <> · {MOD}⏎ {chatTitle(chat, current).slice(0, 18)}</> : null} · ⇥ open in chat · esc
      </div>
    </div>
  );
}
