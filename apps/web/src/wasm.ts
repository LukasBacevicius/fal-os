import init, { CanvasApp } from './wasm/pkg/falos_client';

export type { CanvasApp };

let ready: Promise<unknown> | null = null;

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
  return await CanvasApp.create(canvas, options);
}
