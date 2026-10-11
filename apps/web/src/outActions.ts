// ⌘K entries for "out" and asset ops on the selection (the context menu keeps only the most
// used few). Ops call the host op registry (`POST /api/transform`); placeholders appear on
// the canvas and progress arrives as job events.

import { getState } from './store';
import { isDesktop } from './desktop';
import {
  applyRecipe, copyFalUrl, copyFiles, copyImage, copyPrompt, copyRecipe, download, exportMarked, loadPresets,
  openExport, outNodes, reveal, rerun, runExport, setRole, toggleFinal, toggleLineage, transform, FALLBACK_PRESETS,
} from './out';
import { openPrompt } from './components/SelectionPrompt';

interface Entry { label: string; hint: string; run: () => void }

let presetIds = FALLBACK_PRESETS.map((p) => p.id);

export function outActions(): Entry[] {
  void loadPresets().then((p) => (presetIds = p.map((x) => x.id)));
  const sel = getState().selection;
  const out: Entry[] = [{ label: 'export all marked', hint: 'export', run: () => void exportMarked() }];
  if (!sel.length) return out;
  const media = outNodes(sel);
  const kinds = new Set(media.map((n) => n.kind));
  const image = kinds.has('image');
  const video = kinds.has('video');
  const desk = isDesktop();
  out.push(
    { label: 'export…', hint: '⌘⇧E', run: openExport },
    { label: 'mark final / unmark', hint: 'role', run: () => toggleFinal(sel) },
    { label: 'mark candidate', hint: 'role', run: () => setRole(sel, 'candidate') },
    { label: 'mark rejected', hint: 'role', run: () => setRole(sel, 'rejected') },
    { label: 'show lineage', hint: 'provenance', run: toggleLineage },
    { label: 'rerun', hint: 'provenance', run: () => void rerun() },
    { label: 'rerun with changes…', hint: 'provenance', run: openPrompt },
    { label: 'apply recipe to selection (first → rest)', hint: 'provenance', run: applyRecipe },
    { label: 'copy recipe', hint: 'copy', run: () => copyRecipe() },
  );
  if (media.length) {
    out.push(
      { label: desk ? 'copy file' : 'copy link', hint: '⌘⇧C', run: () => void copyFiles() },
      { label: 'copy fal URL', hint: 'copy', run: () => copyFalUrl() },
      { label: 'copy prompt', hint: 'copy', run: () => copyPrompt() },
      desk ? { label: 'reveal in finder', hint: 'out', run: () => void reveal() } : { label: 'download', hint: 'out', run: () => void download() },
      ...presetIds.map((p) => ({ label: `export as ${p}`, hint: 'quick export', run: () => void runExport(sel, [{ preset: p }]) })),
    );
    if (image) {
      out.push(
        { label: 'copy image', hint: 'copy', run: () => void copyImage() },
        { label: 'remove background', hint: 'op', run: () => void transform('remove_bg') },
        { label: 'upscale', hint: 'op', run: () => void transform('upscale') },
      );
    }
    if (image || video) {
      out.push(
        { label: 'reframe to 9:16', hint: 'op', run: () => void transform('reframe', { aspect: '9:16' }) },
        { label: 'reframe to 1:1', hint: 'op', run: () => void transform('reframe', { aspect: '1:1' }) },
        { label: 'reframe to 4:5', hint: 'op', run: () => void transform('reframe', { aspect: '4:5' }) },
      );
    }
    if (video) {
      out.push(
        { label: 'trim… (asks the chat for in/out)', hint: 'op', run: openPrompt },
        { label: 'extract frames', hint: 'op', run: () => void transform('extract_frames') },
        { label: 'make GIF', hint: 'op', run: () => void transform('gif') },
      );
    }
    for (const f of image ? ['png', 'jpg', 'webp'] : video ? ['mp4', 'webm', 'mov'] : ['wav', 'mp3']) {
      out.push({ label: `convert to ${f}`, hint: 'op', run: () => void transform('convert', { format: f }) });
    }
  }
  return out;
}
