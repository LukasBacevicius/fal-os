import { useEffect, useMemo, useState } from 'react';
import { getState, useStore } from '../store';
import { chatColor, chatTitle, isRunning } from '../chat';
import { openChat } from '../chatActions';
import { workingSet } from '../activity';

/** Pushes the frames agents are working in to the engine (a steady outline in the chat color) and
 *  keeps the reduce-motion setting in sync. Renders nothing. */
export function ActivitySync() {
  const app = useStore((s) => s.app);
  const activity = useStore((s) => s.activity);
  const nodes = useStore((s) => s.nodes);
  const reduce = useStore((s) => s.reduceMotion);
  const working = useMemo(() => workingSet(activity, nodes), [activity, nodes]);
  useEffect(() => {
    app?.setWorking(working);
  }, [app, working]);
  useEffect(() => {
    app?.setReduceMotion(reduce);
  }, [app, reduce]);
  return null;
}

/** Bottom-left of the canvas: one thin colored segment per running chat — what is running,
 *  always visible, even with the panel collapsed. Click opens the chat. */
export function ActivityBars() {
  const chat = useStore((s) => s.chat);
  const [hover, setHover] = useState<string | null>(null);
  const running = chat.order.filter((id) => isRunning(chat, id));
  if (!running.length) return null;
  return (
    <div className="activity-bars">
      {running.map((id) => (
        <button
          key={id}
          className="activity-bar"
          style={{ ['--c' as string]: chatColor(chat, id) }}
          onClick={() => openChat(id)}
          onMouseEnter={() => setHover(id)}
          onMouseLeave={() => setHover(null)}
          aria-label={`open ${chatTitle(chat, id)}`}
        />
      ))}
      {hover && (
        <span className="activity-label">
          {chatTitle(chat, hover)}
          {(() => {
            const a = getState().chat.meta[hover]?.agents ?? [];
            return a.length ? <span className="faint"> · {a.length} agent{a.length === 1 ? '' : 's'}</span> : null;
          })()}
        </span>
      )}
    </div>
  );
}
