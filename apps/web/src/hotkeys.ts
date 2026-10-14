// Layout-independent shortcut matching (docs/research/paper-0515.md §1, keyboard layouts).

/** Keys with a canvas meaning of their own on some layout: a physical-key match never steals
 *  them (e.g. `-` zooms out, and sits where US has `/` on a German keyboard). */
const OWN = new Set(['-', '_', '=', '+', ',', '.', '!', ')', '@', '?', '/', '[', ']', '\\', ' ']);

/** Layout-independent match for punctuation shortcuts (`/ ? [ ] \`): the character, or the
 *  US physical key (`e.code`) when this layout puts a character there that isn't a shortcut
 *  itself (`ü` on German for `[`, `#` for `\`, …). With ⌘ held, macOS reports the base
 *  character, so ⌘[ on German is ⌘ü: the physical key makes it work. */
export function phys(e: Pick<KeyboardEvent, 'key' | 'code'>, ch: string, code: string): boolean {
  if (e.key === ch) return true;
  if (e.code !== code) return false;
  const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  return !OWN.has(k) && !/^[a-z0-9]$/.test(k);
}
