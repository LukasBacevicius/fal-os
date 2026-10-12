// Asset library view: a left-panel tab (⌘⇧A) or full screen (⌘⇧L). A plain mono justified
// grid of host thumbnails, virtualized in bands with hysteresis (rows are materialized a
// screen and a half around the viewport and only re-banded after travelling a quarter
// screen), a search box (words, #hex color, "similar to" from a tile) and a few filter words.
// Keys: arrows move, space quick look, ⏎ places on the canvas (a selection as a grid), f favorite, t tag, c collect,
// u upload to fal, l local-only, s similar, esc closes.

import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { getState, setState, useStore } from '../store';
import {
  ASSET_MIME, GLYPH, VIEWS, collectSelected, findSimilar, hostUrl, lib, libGhost, placeMany, runQuery, setLib, setSearch, setView, tagSelected,
  toggleFavorite, toggleLocalOnly, uploadSelected, useLib, type LibItem,
} from '../library';
import { endDrag, startDrag } from '../dnd';
import { BLANK_DRAG } from './DropLayer';
import { PanelTabs } from './AppMenu';
import { ResizeHandle } from './ResizeHandle';
import { emptySpot } from '../controller';
import { ItemActions, Viewer } from './Preview';
import { kindLabel, previewKind, viewerKeys, type PreviewItem } from '../preview';
import { modelPoster } from '../cardArt';
import { bytes as fmtBytes } from '../format';

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

/** Library thumbnails exist for PDFs, SVGs and fonts too (host: pdftoppm / Quick Look). */
const docThumb = (it: LibItem) => /^(application\/pdf|image\/svg|font\/)/.test(it.mime ?? '') || /\.(pdf|svg|ttf|otf|woff2?)$/i.test(it.name ?? '');

/** A tile without a raster thumbnail: a 3D poster rendered in the page, else a file card. */
function TileCard({ it }: { it: LibItem }) {
  const name = it.name || it.sha.slice(0, 12);
  const kind = previewKind(it.mime ?? '', name, it.kind === 'model3d' ? 'model3d' : undefined);
  const [poster, setPoster] = useState<string | null>(null);
  useEffect(() => {
    if (kind !== 'model3d' || !it.src) return;
    let url: string | null = null;
    let live = true;
    void modelPoster(hostUrl(it.src), `sha256:${it.sha}`).then((b) => {
      if (!live || !b) return;
      url = URL.createObjectURL(b);
      setPoster(url);
    });
    return () => {
      live = false;
      if (url) URL.revokeObjectURL(url);
    };
  }, [kind, it.src, it.sha]);
  if (poster) return <img src={poster} alt="" draggable={false} className="contain" />;
  return (
    <div className="lib-box">
      <div><span className="dim">[{kindLabel(kind, it.mime ?? '', name)}]</span> {name}</div>
      {it.bytes ? <div className="faint">{fmtBytes(it.bytes)}</div> : null}
    </div>
  );
}

const Tile = memo(function Tile({ it, x, y, w, h, on, cursor, index }: { it: LibItem; x: number; y: number; w: number; h: number; on: boolean; cursor: boolean; index: number }) {
  const [scrub, setScrub] = useState<number | null>(null);
  // A thumbnail still being generated answers 503: retry a few times.
  const [retry, setRetry] = useState(0);
  const [failed, setFailed] = useState(false);
  const thumb = hostUrl(it.thumb) + (retry ? `&r=${retry}` : '');
  const glyph = GLYPH[it.source] ?? '';
  return (
    <div
      className={`lib-tile${on ? ' on' : ''}${cursor ? ' cur' : ''}`}
      style={{ transform: `translate(${x}px,${y}px)`, width: w, height: h }}
      data-i={index}
      draggable
      title="drag onto the canvas or the chat · double-click places (⏎) · space quick look"
      onDragStart={(e) => {
        // Dragging a selected tile takes the whole selection along.
        const s = lib();
        const shas = s.selected.includes(it.sha) ? s.selected : [it.sha];
        if (!s.selected.includes(it.sha)) setLib({ selected: [it.sha], cursor: index });
        e.dataTransfer.setData(ASSET_MIME, shas.join('\n'));
        e.dataTransfer.setData('text/plain', it.prompt ?? it.sha);
        e.dataTransfer.effectAllowed = 'copy';
        // The thumbnail follows the pointer as our own DOM ghost (true size on the canvas).
        e.dataTransfer.setDragImage(BLANK_DRAG, 0, 0);
        startDrag({ source: 'library', shas, items: shas.map(libGhost) });
      }}
      onDragEnd={() => endDrag()}
      onMouseMove={it.strip ? (e) => {
        const r = e.currentTarget.getBoundingClientRect();
        setScrub(Math.min(11, Math.max(0, Math.floor(((e.clientX - r.left) / r.width) * 12))));
      } : undefined}
      onMouseLeave={it.strip ? () => setScrub(null) : undefined}
    >
      {it.kind === 'model3d' || (it.kind === 'other' && !docThumb(it)) || it.thumb_state === 'none' || failed ? (
        <TileCard it={it} />
      ) : scrub !== null ? (
        <div className="lib-strip" style={{ backgroundImage: `url("${hostUrl(it.strip)}")`, backgroundPosition: `${((scrub % 4) / 3) * 100}% ${(Math.floor(scrub / 4) / 2) * 100}%` }} />
      ) : (
        <img
          src={thumb}
          alt=""
          loading="lazy"
          decoding="async"
          draggable={false}
          className={it.kind === 'other' ? 'contain' : undefined}
          onError={() => (retry < 6 ? window.setTimeout(() => setRetry((r) => r + 1), 1500 * (retry + 1)) : setFailed(true))}
        />
      )}
      <span className="lib-badge">
        {glyph}
        {it.fal?.synced && it.source === 'import' ? '↑' : ''}
        {it.role === 'final' ? ' ★' : ''}
        {it.favorite ? ' ♥' : ''}
        {(it.kind === 'video' || it.kind === 'audio') && it.duration ? ` ${it.kind === 'audio' ? '♪ ' : ''}${fmtDur(it.duration)}` : ''}
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
      if (!t) return;
      const sha = items[Number(t.dataset.i)].sha;
      const sel = lib().selected;
      placeMany(sel.includes(sha) ? sel : [sha]);
    }}>
      <div className="lib-space" style={{ height: L.height }}>{tiles}</div>
    </div>
  );
}

/** A library item as a preview item (originals stream from the host or the fal CDN). */
function libItem(it: LibItem): PreviewItem {
  const name = it.name || it.sha.slice(0, 12);
  const local = !!it.src && it.src.startsWith('/assets/');
  const sample = lib().items.find((x) => x.kind === 'image' && x.thumb_state === 'ok');
  return {
    key: `lib:${it.sha}`,
    kind: previewKind(it.mime ?? '', name, it.kind === 'model3d' ? 'model3d' : undefined),
    name,
    mime: it.mime ?? '',
    url: hostUrl(it.src),
    poster: it.thumb_state === 'none' ? undefined : hostUrl(it.thumb_lg),
    sha: local && it.sha.length === 64 ? it.sha : undefined,
    bytes: it.bytes ?? null,
    w: it.w,
    h: it.h,
    duration: it.duration,
    sample: sample ? hostUrl(sample.thumb_lg) : undefined,
  };
}

function QuickLook() {
  const it = useLib((s) => s.items[s.cursor]);
  const item = useMemo(() => (it ? libItem(it) : null), [it]);
  if (!it || !item) return null;
  const dims = it.w && it.h ? `${it.w}×${it.h}` : '';
  return (
    <div className="lib-ql" onClick={(e) => e.target === e.currentTarget && setLib({ quick: false })}>
      <div className="lib-ql-media" key={item.key}><Viewer item={item} /></div>
      <div className="lib-ql-info">
        <div>
          {[GLYPH[it.source] + ' ' + it.source, it.kind, dims, it.duration ? fmtDur(it.duration) : '', it.model ?? '', it.role ?? ''].filter(Boolean).join(' · ')}
          {it.fal?.synced ? ' · ↑fal' : ''}
          <span className="lib-ql-actions" onMouseDown={(e) => e.preventDefault()}>
            <ItemActions item={item} />
            <button className="link" title="close (space / esc)" onClick={() => setLib({ quick: false })}>[×]</button>
          </span>
        </div>
        {(it.prompt || it.caption) && <div className="dim lib-ql-prompt">{it.prompt ?? it.caption}</div>}
        {it.lineage && <div className="dim">{it.lineage}</div>}
        <div className="faint">{[it.palette?.join(' '), it.tags.length ? '#' + it.tags.join(' #') : '', it.projects.join(', '), it.sha.slice(0, 12)].filter(Boolean).join(' · ')}</div>
      </div>
    </div>
  );
}

/** Click actions for the selected tiles (each shows its key): place, quick look, favorite,
 *  tag, collect, upload to fal, local-only, similar, attach to the chat. */
function LibActions() {
  const selected = useLib((s) => s.selected);
  const quick = useLib((s) => s.quick);
  const fav = useLib((s) => s.items.find((i) => i.sha === s.selected[0])?.favorite ?? false);
  const local = useLib((s) => s.items.find((i) => i.sha === s.selected[0])?.local_only ?? false);
  if (!selected.length) return null;
  const toChat = () => {
    const app = getState().app;
    if (!app) return;
    const [x, y] = emptySpot();
    const [wx, wy] = app.screenToWorld(x, y);
    placeMany(selected, [wx, wy], { attach: true });
    setState({ chatOpen: true });
  };
  return (
    <div className="lib-actions">
      <span className="faint">{selected.length > 1 ? `${selected.length} selected` : '1 selected'}</span>
      <button className="link" title="place on the canvas (⏎ · double-click · drag)" onClick={() => placeMany(selected)}>[place]</button>
      <button className="link dim" title="attach to the chat (or drag onto it)" onClick={toChat}>[+ chat]</button>
      <button className={`link ${quick ? '' : 'dim'}`} title="quick look (space)" onClick={() => setLib({ quick: !quick })}>[look]</button>
      <button className="link dim" title={fav ? 'unfavorite (f)' : 'favorite (f)'} onClick={toggleFavorite}>{fav ? '[♥]' : '[♡]'}</button>
      <button className="link dim" title="tag (t)" onClick={tagSelected}>[tag]</button>
      <button className="link dim" title="add to a collection (c)" onClick={collectSelected}>[collect]</button>
      <button className="link dim" title="upload to your fal library (u)" onClick={uploadSelected}>[upload]</button>
      <button className="link dim" title={local ? 'allow upload to fal (l)' : 'keep local only (l)'} onClick={toggleLocalOnly}>{local ? '[local ✓]' : '[local]'}</button>
      <button className="link dim" title="find similar (s)" onClick={() => findSimilar(lib().items[lib().cursor]?.sha ?? selected[0])}>[similar]</button>
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
    // The quick look's viewer keys (zoom, pages, transport) first.
    if (s.quick && !inInput && !['Escape', ' ', 'Enter', 'ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(k) && viewerKeys.handle?.(e.nativeEvent)) {
      e.preventDefault();
      return;
    }
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
      placeMany(shas);
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
      {mode === 'panel' && <ResizeHandle panel="library" edge="right" />}
      {mode === 'panel' && <PanelTabs />}
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
        {mode === 'full' && <button className="link dim" onClick={() => setState({ library: 'panel' })} title="back to the left panel (⌘⇧A)">[⤡]</button>}
        {mode === 'full' && <button className="link dim" onClick={() => setState({ library: null })} title="close (esc)">[×]</button>}
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
      <LibActions />
      {quick && <QuickLook />}
    </div>
  );
}
