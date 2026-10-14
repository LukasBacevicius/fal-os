import { dismissNotice, useStore, type Notice } from '../store';

/** The notice stack: dim one-line notices at the bottom, newest last. Successes fade,
 *  failures stay until [×], ongoing conditions are one line updated in place. */
export function Notices() {
  const notices = useStore((s) => s.notices);
  if (!notices.length) return null;
  return (
    <div className="notices" role="status" aria-live="polite">
      {notices.map((n) => <NoticeLine key={n.key} n={n} />)}
    </div>
  );
}

function NoticeLine({ n }: { n: Notice }) {
  return (
    <div className={`notice notice-${n.level}`}>
      <span className="notice-text">{n.text}</span>
      {n.actions.map((a) => (
        <button key={a.label} className="link dim notice-action" onClick={a.run}>[{a.label}]</button>
      ))}
      {n.level === 'error' && (
        <button
          className="link faint notice-action"
          aria-label="dismiss"
          onClick={() => {
            n.onDismiss?.();
            dismissNotice(n.key);
          }}
        >
          [×]
        </button>
      )}
    </div>
  );
}

/** Cards: something stopped and needs a decision. One plain line with its actions. */
export function Cards() {
  const cards = useStore((s) => s.cards);
  if (!cards.length) return null;
  return (
    <div className="cards">
      {cards.map((c) => (
        <div key={c.key} className="card" role="alert">
          <span>{c.text}</span>
          {c.actions.map((a) => (
            <button key={a.label} className="link dim notice-action" onClick={a.run}>[{a.label}]</button>
          ))}
        </div>
      ))}
    </div>
  );
}
