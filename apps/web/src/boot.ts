// Startup watchdog: never a blank or hung window. Each boot step has a time budget; a step
// that overruns shows a card naming it with [retry] (a reload) instead of nothing. Steps:
// `wasm` (the canvas engine), `host` (finding the fal OS host), `project` (the project's
// first snapshot). A `boot_phases` line with each step's time goes to the console.

import { hideCard, showCard } from './store';

export type BootStep = 'wasm' | 'host' | 'project';

const BUDGET_MS: Record<BootStep, number> = { wasm: 10_000, host: 25_000, project: 15_000 };
const LABEL: Record<BootStep, string> = {
  wasm: 'loading the canvas engine',
  host: 'finding the fal OS host',
  project: 'opening the project',
};

const timers = new Map<BootStep, number>();
const started = new Map<BootStep, number>();
const phases: Record<string, number> = {};
let stalled: BootStep | null = null;

export function bootStart(step: BootStep) {
  window.clearTimeout(timers.get(step));
  started.set(step, performance.now());
  timers.set(
    step,
    window.setTimeout(() => {
      stalled = step;
      console.warn(`boot: ${step} is past its ${BUDGET_MS[step] / 1000}s budget`);
      showCard('boot', `stuck ${LABEL[step]}`, [{ label: 'retry', run: () => location.reload() }]);
    }, BUDGET_MS[step]),
  );
}

export function bootDone(step: BootStep) {
  const t = timers.get(step);
  if (t === undefined) return;
  window.clearTimeout(t);
  timers.delete(step);
  phases[step] = Math.round(performance.now() - (started.get(step) ?? 0));
  if (stalled === step) {
    stalled = null;
    hideCard('boot');
  }
  if (step === 'project' || (step === 'host' && !timers.size)) console.info('boot_phases', JSON.stringify(phases));
}

/** A step that won't happen (no host: the canvas stays local). */
export function bootSkip(step: BootStep) {
  window.clearTimeout(timers.get(step));
  timers.delete(step);
  if (stalled === step) {
    stalled = null;
    hideCard('boot');
  }
}
