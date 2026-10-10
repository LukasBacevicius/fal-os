/** `12.345` → `0:12.34`. */
export function timecode(t: number): string {
  const m = Math.floor(t / 60);
  const s = t - m * 60;
  return `${m}:${s.toFixed(2).padStart(5, '0')}`;
}

/** `0.4213` → `$0.42`. */
export function usd(v: number): string {
  return `$${v.toFixed(2)}`;
}
