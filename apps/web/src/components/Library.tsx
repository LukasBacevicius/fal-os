// Asset library view: a left-panel tab (⌘⇧A) or full screen (⌘⇧L). A plain mono justified
// grid of host thumbnails, virtualized in bands with hysteresis (rows are materialized a
// screen and a half around the viewport and only re-banded after travelling a quarter
// screen), a search box (words, #hex color, "similar to" from a tile) and a few filter words.
// Keys: arrows move, space quick look, ⏎ places on the canvas, f favorite, t tag, c collect,
// u upload to fal, l local-only, s similar, esc closes.

import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { setState, useStore } from '../store';
import {
  ASSET_MIME, GLYPH, VIEWS, collectSelected, findSimilar, hostUrl, lib, place, runQuery, setLib, setSearch, setView, tagSelected,
  toggleFavorite, toggleLocalOnly, uploadSelected, useLib, type LibItem,
} from '../library';

const GAP = 4;

export interface Layout { x: Float64Array; y: Float64Array; w: Float64Array; h: Float64Array; rows: { y: number; h: number; start: number; end: number }[]; height: number }

const aspect = (i: LibItem) => (i.kind === 'audio' ? 2 : i.w && i.h ? Math.min(4, Math.max(0.25, i.w / i.h)) : 1);

/** Justified rows: fill each row to the width at about `rowH`; the last row keeps `rowH`. */
export function justify(items: LibItem[], width: number, rowH: number): Layout {
  const n = items.length;
  const L: Layout = { x: new Float64Array(n), y: new Float64Array(n), w: new Float64Array(n), h: new Float64Array(n), rows: [], height: 0 };
  let y = 0;
  let start = 0;
  let sum = 0;
  const flush = (end: number, last: boolean) => {
    const gaps = GAP * (end - start - 1);
    let h = last ? rowH : (width - gaps) / sum;
    if (!last) h = Math.min(h, rowH * 1.6);
    let x = 0;
    for (let i = start; i < end; i++) {
      const w = aspect(items[i]) * h;
      L.x[i] = x;
      L.y[i] = y;
      L.w[i] = w;
      L.h[i] = h;
      x += w + GAP;
    }
    L.rows.push({ y, h, start, end });
    y += h + GAP;
  };
  for (let i = 0; i < n; i++) {
    sum += aspect(items[i]);
    if (sum * rowH + GAP * (i - start) >= width) {
      flush(i + 1, false);
      start = i + 1;
      sum = 0;
    }
  }
  if (start < n) flush(n, true);
  L.height = y;
  return L;
}

/** First row whose bottom is below `y`. */
function rowAt(L: Layout, y: number): number {
  let lo = 0;
  let hi = L.rows.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (L.rows[mid].y + L.rows[mid].h < y) lo = mid + 1;
    else hi = mid;
  }
  return Math.max(0, lo);
}

const fmtDur = (d: number) => `${Math.floor(d / 60)}:${String(Math.floor(d % 60)).padStart(2, '0')}`;

const Tile = memo(function Tile({ it, x, y, w, h, on, cursor, index }: { it: LibItem; x: number; y: number; w: number; h: number; on: boolean; cursor: boolean; index: number }) {
  const [scrub, setScrub] = useState<number | null>(null);
  // A thumbnail still being generated answers 503: retry a few times.
  const [retry, setRetry] = useState(0);
  const thumb = hostUrl(it.thumb) + (retry ? `&r=${retry}` : '');
  const glyph = GLYPH[it.source] ?? '';
  return (
    <div
      className={`lib-tile${on ? ' on' : ''}${cursor ? ' cur' : ''}`}
      style={{ transform: `translate(${x}px,${y}px)`, width: w, height: h }}
      data-i={index}
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData(ASSET_MIME, it.sha);
        e.dataTransfer.setData('text/plain', it.prompt ?? it.sha);
        e.dataTransfer.effectAllowed = 'copy';
      }}
      onMouseMove={it.strip ? (e) => {
        const r = e.currentTarget.getBoundingClientRect();
        setScrub(Math.min(11, Math.max(0, Math.floor(((e.clientX - r.left) / r.width) * 12))));
      } : undefined}
      onMouseLeave={it.strip ? () => setScrub(null) : undefined}
    >
      {it.kind === 'other' || it.kind === 'model3d' || it.thumb_state === 'none' ? (
        <div className="lib-box dim">[{it.kind === 'model3d' ? '3d' : (it.mime?.split('/')[1] ?? it.kind)}] {it.name ?? ''}</div>
      ) : scrub !== null ? (
        <div className="lib-strip" style={{ backgroundImage: `url("${hostUrl(it.strip)}")`, backgroundPosition: `${((scrub % 4) / 3) * 100}% ${(Math.floor(scrub / 4) / 2) * 100}%` }} />
      ) : (
        <img src={thumb} alt="" loading="lazy" decoding="async" draggable={false} onError={() => retry < 6 && window.setTimeout(() => setRetry((r) => r + 1), 1500 * (retry + 1))} />
      )}
      <span className="lib-badge">
        {glyph}
        {it.fal?.synced && it.source === 'import' ? '↑' : ''}
        {it.role === 'final' ? ' ★' : ''}
        {it.favorite ? ' ♥' : ''}
        {it.kind === 'video' && it.duration ? ` ${fmtDur(it.duration)}` : ''}
      </span>
    </div>
  );
});

function Grid({ rowH }: { rowH: number }) {
  const items = useLib((s) => s.items);
  const selected = useLib((s) => s.selected);
  const cursor = useLib((s) => s.cursor);
  const el = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [viewH, setViewH] = useState(600);
  const [band, setBand] = useState<[number, number]>([0, 0]);
  const L = useMemo(() => justify(items, Math.max(100, width), rowH), [items, width, rowH]);
  const sel = useMemo(() => new Set(selected), [selected]);

  useLayoutEffect(() => {
    const e = el.current!;
    const ro = new ResizeObserver(() => {
      setWidth(e.clientWidth - 2);
      setViewH(e.clientHeight);
    });
    ro.observe(e);
    return () => ro.disconnect();
  }, []);

  // Band virtualization with hysteresis: re-band only when the viewport gets within a
  // quarter screen of the band's edge (travel-triggered, not every scroll event).
  const reband = (force = false) => {
    const e = el.current;
    if (!e) return;
    const top = e.scrollTop;
    const bottom = top + e.clientHeight;
    const h = e.clientHeight || 600;
    setBand((b) => {
      const covered = b[1] > b[0] && top - h * 0.25 >= b[0] && bottom + h * 0.25 <= b[1];
      if (covered && !force) return b;
      return [Math.max(0, top - h * 1.5), bottom + h * 1.5];
    });
  };
  useEffect(() => reband(true), [L, viewH]);
  const queryKey = useLib((s) => `${s.view}|${s.search}|${s.similar}`);
  useEffect(() => {
    el.current?.scrollTo({ top: 0 });
  }, [queryKey]);

  // Keep the cursor tile in view.
  useEffect(() => {
    const e = el.current;
    if (!e || cursor >= items.length) return;
    const y = L.y[cursor];
    const h = L.h[cursor];
    if (y < e.scrollTop) e.scrollTop = y - GAP;
    else if (y + h > e.scrollTop + e.clientHeight) e.scrollTop = y + h - e.clientHeight + GAP;
  }, [cursor, L, items.length]);

  const tiles: React.ReactNode[] = [];
  if (L.rows.length) {
    const r0 = rowAt(L, band[0]);
    for (let r = r0; r < L.rows.length && L.rows[r].y <= band[1]; r++) {
      const row = L.rows[r];
      for (let i = row.start; i < row.end; i++) {
        const it = items[i];
        tiles.push(<Tile key={it.sha} it={it} index={i} x={L.x[i]} y={L.y[i]} w={L.w[i]} h={L.h[i]} on={sel.has(it.sha)} cursor={i === cursor} />);
      }
    }
  }

  const pick = (e: React.MouseEvent) => {
    const t = (e.target as HTMLElement).closest('.lib-tile') as HTMLElement | null;
    if (!t) return setLib({ selected: [] });
    const i = Number(t.dataset.i);
    const sha = items[i]?.sha;
    if (!sha) return;
    if (e.shiftKey) {
      const a = Math.min(lib().cursor, i);
      const b = Math.max(lib().cursor, i);
      setLib({ selected: [...new Set([...lib().selected, ...items.slice(a, b + 1).map((x) => x.sha)])], cursor: i });
    } else if (e.metaKey || e.ctrlKey) {
      const s = new Set(lib().selected);
      if (s.has(sha)) s.delete(sha);
      else s.add(sha);
      setLib({ selected: [...s], cursor: i });
    } else setLib({ selected: [sha], cursor: i });
  };

  return (
    <div className="lib-grid" ref={el} onScroll={() => reband()} onClick={pick} onDoubleClick={(e) => {
      const t = (e.target as HTMLElement).closest('.lib-tile') as HTMLElement | null;
      if (t) place(items[Number(t.dataset.i)].sha);
    }}>
      <div className="lib-space" style={{ height: L.height }}>{tiles}</div>
    </div>
  );
}

function QuickLook() {
  const it = useLib((s) => s.items[s.cursor]);
  if (!it) return null;
  const big = it.kind === 'video' && it.src ? (
    <video src={hostUrl(it.src)} autoPlay loop muted playsInline poster={hostUrl(it.thumb_lg)} />
  ) : it.kind === 'audio' && it.src ? (
    <div className="lib-ql-audio"><img src={hostUrl(it.thumb_lg)} alt="" /><audio src={hostUrl(it.src)} controls autoPlay /></div>
  ) : (
    <img src={hostUrl(it.thumb_lg)} alt="" />
  );
  const dims = it.w && it.h ? `${it.w}×${it.h}` : '';
  return (
    <div className="lib-ql" onClick={() => setLib({ quick: false })}>
      <div className="lib-ql-media">{big}</div>
      <div className="lib-ql-info">
        <div>{[GLYPH[it.source] + ' ' + it.source, it.kind, dims, it.duration ? fmtDur(it.duration) : '', it.model ?? '', it.role ?? ''].filter(Boolean).join(' · ')}{it.fal?.synced ? ' · ↑fal' : ''}</div>
        {(it.prompt || it.caption) && <div className="dim lib-ql-prompt">{it.prompt ?? it.caption}</div>}
        <div className="faint">{[it.palette?.join(' '), it.tags.length ? '#' + it.tags.join(' #') : '', it.projects.join(', '), it.sha.slice(0, 12)].filter(Boolean).join(' · ')}</div>
      </div>
    </div>
  );
}

export function Library() {
  const mode = useStore((s) => s.library);
  const view = useLib((s) => s.view);
  const search = useLib((s) => s.search);
  const similar = useLib((s) => s.similar);
  const loading = useLib((s) => s.loading);
  const quick = useLib((s) => s.quick);
  const empty = useLib((s) => s.items.length === 0);
  const fal = useLib((s) => s.fal);
  const writes = useLib((s) => s.writes);
  const root = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    runQuery();
    if (mode === 'full') requestAnimationFrame(() => input.current?.focus());
  }, [mode]);

  const cols = () => {
    // Tiles per row around the cursor, for ↑/↓.
    const g = root.current?.querySelector('.lib-grid') as HTMLElement | null;
    return Math.max(1, Math.round((g?.clientWidth ?? 600) / (mode === 'full' ? 260 : 150)));
  };

  const onKey = (e: React.KeyboardEvent) => {
    const s = lib();
    const inInput = e.target === input.current;
    const k = e.key;
    const move = (d: number) => {
      e.preventDefault();
      const c = Math.max(0, Math.min(s.items.length - 1, s.cursor + d));
      setLib({ cursor: c, selected: e.shiftKey ? [...new Set([...s.selected, s.items[c]?.sha].filter(Boolean))] : s.items[c] ? [s.items[c].sha] : [] });
    };
    if (k === 'Escape') {
      e.preventDefault();
      if (s.quick) return setLib({ quick: false });
      if (inInput && s.search) return setSearch('');
      if (s.selected.length > 1) return setLib({ selected: [] });
      return setState({ library: null });
    }
    if (inInput) {
      if (k === 'ArrowDown' || k === 'Enter') {
        e.preventDefault();
        root.current?.focus();
        if (!s.selected.length && s.items[0]) setLib({ cursor: 0, selected: [s.items[0].sha] });
      }
      return;
    }
    if (k === 'ArrowRight') return move(1);
    if (k === 'ArrowLeft') return move(-1);
    if (k === 'ArrowDown') return move(cols());
    if (k === 'ArrowUp') {
      if (s.cursor < cols()) return (e.preventDefault(), input.current?.focus());
      return move(-cols());
    }
    if (k === ' ') return (e.preventDefault(), setLib({ quick: !s.quick }));
    if (k === 'Enter') {
      e.preventDefault();
      const shas = s.selected.length ? s.selected : s.items[s.cursor] ? [s.items[s.cursor].sha] : [];
      shas.forEach((sha) => place(sha));
      return;
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (k === 'f') return toggleFavorite();
    if (k === 't') return tagSelected();
    if (k === 'c') return collectSelected();
    if (k === 'u') return uploadSelected();
    if (k === 'l') return toggleLocalOnly();
    if (k === 's' && s.items[s.cursor]) return findSimilar(s.items[s.cursor].sha);
    if (k === '/') return (e.preventDefault(), input.current?.focus());
  };

  if (!mode) return null;
  return (
    <div className={`library ${mode}`} ref={root} tabIndex={-1} onKeyDown={onKey}>
      <div className="lib-head">
        <input
          ref={input}
          className="lib-search"
          value={similar ? `similar ${similar.slice(0, 8)}` : search}
          placeholder="search · #hex"
          spellCheck={false}
          onChange={(e) => setSearch(e.target.value.replace(/^similar \S*\s?/, ''))}
        />
        {loading && <span className="faint">…</span>}
        <button className="link dim" onClick={() => setState({ library: mode === 'full' ? 'panel' : 'full' })} title="⌘⇧A panel · ⌘⇧L full">{mode === 'full' ? '[–]' : '[+]'}</button>
        <button className="link dim" onClick={() => setState({ library: null })}>[×]</button>
      </div>
      <div className="lib-views">
        {VIEWS.map((v) => (
          <button key={v} className={`link${v === view ? ' on' : ' dim'}`} onClick={() => setView(v)}>{v === 'fav' ? '♥' : v}</button>
        ))}
        {view === 'shown' && <button className="link on">shown</button>}
      </div>
      {(fal === 'off' || writes === 'unavailable') && <div className="lib-note faint">{fal === 'off' ? 'fal not connected: local library only' : 'fal writes not available'}</div>}
      <Grid rowH={mode === 'full' ? 200 : 96} />
      {empty && !loading && <div className="lib-empty faint">{search || similar ? 'nothing matches' : 'no assets yet'}</div>}
      {quick && <QuickLook />}
    </div>
  );
}
