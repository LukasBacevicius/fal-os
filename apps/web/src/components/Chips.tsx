import { useState } from 'react';
import { getState, useStore } from '../store';
import { focusNode } from '../controller';
import { chatColor, chatTitle, chipLabel, type Chip } from '../chat';
import { openChat } from '../chatActions';
import type { NodeView } from '../types';

/** Object/remote URL for an image node's pixels (null when not loaded or not an image). */
export function thumbUrl(n: NodeView | undefined | null): string | null {
  if (!n || n.kind !== 'image') return null;
  const asset = typeof n.props?.asset === 'string' ? (n.props.asset as string) : null;
  if (!asset) return null;
  return getState().app?.assetUrl(asset) ?? null;
}

/** A larger preview next to a hovered chip (fixed, so it escapes scrolling containers). */
function Preview({ url, at }: { url: string; at: DOMRect }) {
  const size = 168;
  const top = at.top - size - 6 > 4 ? at.top - size - 6 : at.bottom + 6;
  const left = Math.max(4, Math.min(innerWidth - size - 4, at.left));
  return <img className="chip-preview" src={url} alt="" style={{ top, left, width: size, height: size }} />;
}

function ChipView({ c, nodes, onRemove }: { c: Chip; nodes: Map<string, NodeView>; onRemove?: () => void }) {
  const chat = useStore((s) => s.chat);
  const [hover, setHover] = useState<DOMRect | null>(null);
  const n = c.kind === 'chat' ? null : nodes.get(c.node);
  const url = c.kind === 'node' ? thumbUrl(n) : null;
  const go = () => (c.kind === 'chat' ? openChat(c.chat) : n && focusNode(n.id));
  const label = chipLabel(c, n, c.kind === 'chat' ? chatTitle(chat, c.chat) : undefined);
  if (url) {
    return (
      <span
        className="chip chip-img"
        onMouseEnter={(e) => setHover(e.currentTarget.getBoundingClientRect())}
        onMouseLeave={() => setHover(null)}
      >
        <button className="link chip-img-btn" onClick={go} title={n ? `${n.name || n.id} · pan to it` : ''}>
          <img src={url} alt="" width={24} height={24} />
          <span className="chip-img-name">{label.replace(/^img /, '')}</span>
        </button>
        {onRemove && <button className="link chip-x" onClick={onRemove} title="remove">×</button>}
        {hover && <Preview url={url} at={hover} />}
      </span>
    );
  }
  return (
    <span className={`chip ${n || c.kind === 'chat' ? '' : 'gone'}`}>
      {c.kind === 'chat' && <span className="swatch" style={{ background: chatColor(chat, c.chat) }} />}
      <button className="link" onClick={go}>[{label}</button>
      {onRemove && <button className="link chip-x" onClick={onRemove} title="remove">×</button>}
      ]
    </span>
  );
}

/** Context chips: image chips show a 24 px thumbnail (hover: preview), others are bracketed
 *  text. Click pans to the node (or opens the chat). */
export function Chips({ chips, onRemove }: { chips: Chip[]; onRemove?: (i: number) => void }) {
  const list = useStore((s) => s.nodes);
  if (!chips.length) return null;
  const nodes = new Map(list.map((n) => [n.id, n]));
  return (
    <div className="chips">
      {chips.map((c, i) => <ChipView key={`${i}-${JSON.stringify(c)}`} c={c} nodes={nodes} onRemove={onRemove ? () => onRemove(i) : undefined} />)}
    </div>
  );
}
