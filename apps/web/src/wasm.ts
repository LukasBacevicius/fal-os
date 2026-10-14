import init, { CanvasApp } from './wasm/pkg/falos_client';

export type { CanvasApp };

let ready: Promise<unknown> | null = null;
let blank: Uint8Array | null = null;

/** An empty canvas (taken from the fresh client): what a project shows before its host
 *  answers, instead of the previous project's nodes. */
export const blankSnapshot = () => blank;

/** Loads the wasm module once and creates the canvas client on `canvas`. */
export async function createCanvasApp(
  canvas: HTMLCanvasElement,
  options: { actor: { id: string; name: string }; canvas: { id: string; name: string } },
): Promise<CanvasApp> {
  if (!('gpu' in navigator)) {
    throw new Error('WebGPU is not available in this browser.');
  }
  ready ??= init();
  await ready;
  const app = await CanvasApp.create(canvas, options);
  try {
    blank ??= app.exportSnapshot();
  } catch {
    /* no blank: switching keeps the old scene until the host answers */
  }
  return app;
}
