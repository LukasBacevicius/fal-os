import { useEffect, useRef, useState } from 'react';
import { setState, toast, useStore } from '../store';
import { AGENTS, actorById } from '../agents';
import { anchorLabel, timecode } from '../format';
import { IconClock, IconSend } from '../icons';
import { Avatar } from './Avatar';
import type { Anchor } from '../types';

/** Popover for the annotation being placed with the annotate tool. */
export function Composer() {
  const app = useStore((s) => s.app);
  const draft = useStore((s) => s.draft);
  const camera = useStore((s) => s.camera);
  const nodes = useStore((s) => s.nodes);
  const [text, setText] = useState('');
  const [assignee, setAssignee] = useState<string | null>(null);
  const area = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (draft) {
      setText('');
      setAssignee(null);
      area.current?.focus({ preventScroll: true });
    }
  }, [draft?.nodeId, draft?.region.kind, draft && JSON.stringify(draft.region)]);

  if (!app || !draft || !draft.screen) return null;
  const node = nodes.find((n) => n.id === draft.nodeId);
  const anchor: Anchor = draft.time != null ? { kind: 'frame', t: draft.time, region: draft.region } : draft.region;
  // Beside the pin (right of the region); else left of the region; else below it.
  const W = 300, H = 230, gap = 12;
  const [sx, sy] = draft.screen;
  const [rx, ry, rw, rh] = draft.rect ?? [sx, sy, 0, 0];
  let left = Math.max(sx, rx + rw) + 36;
  let top = sy - 34;
  if (left + W > camera.width - gap) {
    left = rx - W - gap - (rw === 0 ? 8 : 0);
    if (left < gap) {
      left = Math.min(Math.max(gap, rx), camera.width - W - gap);
      top = ry + rh + gap;
    }
  }
  top = Math.min(Math.max(gap, top), Math.max(gap, camera.height - H));

  const mention = (t: string) => AGENTS.find((a) => new RegExp(`@${a.id}\\b`, 'i').test(t))?.id ?? null;
  const submit = () => {
    if (!text.trim()) return;
    try {
      app.submitDraft(text.trim(), assignee ?? mention(text));
      setState({ draft: null });
    } catch (e) {
      toast(String(e));
    }
  };

  return (
    <div className="composer" style={{ left, top, width: W }} onPointerDown={(e) => e.stopPropagation()}>
      <div className="composer-head">
        <span className="composer-title">New annotation</span>
        <span className="chip">
          {draft.time != null && <IconClock size={12} />}
          {draft.time != null ? timecode(draft.time) : anchorLabel(anchor)}
        </span>
      </div>
      {node && <div className="composer-target">on {node.name || node.kind}</div>}
      <textarea
        ref={area}
        className="composer-input"
        placeholder="Describe the change… mention @claude, @codex or @fal-agent"
        value={text}
        rows={3}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            submit();
          } else if (e.key === 'Escape') {
            app.cancelDraft();
          }
        }}
      />
      <div className="composer-assign">
        <span className="label">Assign</span>
        {AGENTS.map((a) => (
          <button
            key={a.id}
            className={`assign-pill ${(assignee ?? mention(text)) === a.id ? 'on' : ''}`}
            onClick={() => setAssignee(assignee === a.id ? null : a.id)}
            title={`Assign to ${a.name}`}
          >
            <Avatar actor={actorById(a.id)} size={16} />
            {a.name}
          </button>
        ))}
      </div>
      <div className="composer-actions">
        <button className="btn ghost" onClick={() => app.cancelDraft()}>Cancel</button>
        <button className="btn primary" disabled={!text.trim()} onClick={submit}>
          Post <IconSend size={13} />
        </button>
      </div>
    </div>
  );
}
