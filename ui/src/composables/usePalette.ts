import { computed, ref } from "vue";

/**
 * Whether the command palette is open, shared by the header button, the admin
 * sidebar button and the keyboard shortcut (RFC 0046 §4.2). `requested` turns
 * true on the first open and stays so: the palette is a lazy chunk, fetched
 * only once someone asks for it.
 */
const open = ref(false);
const requested = ref(false);

function openPalette() {
  requested.value = true;
  open.value = true;
}

/** `⌘K` on macOS, `Ctrl K` elsewhere: the i18n key of the chord the button shows. */
const isMac =
  typeof navigator !== "undefined" &&
  /mac|iphone|ipad/i.test(navigator.platform || navigator.userAgent);
const chord = computed(() => (isMac ? "palette.chordMac" : "palette.chordCtrl"));

/**
 * The shortcut's one rule (§4.2, §7.1 T6): it works from a single-line input —
 * the catalog search included, where the browser's own `Ctrl+K` would take focus
 * to the address bar — and does nothing in a `textarea` or `contenteditable`,
 * where an operator may be typing a configuration for minutes.
 */
export function isPaletteShortcut(event: KeyboardEvent): boolean {
  if (event.key.toLowerCase() !== "k" || event.altKey || event.shiftKey) return false;
  if (!(event.ctrlKey || event.metaKey)) return false;
  const target = event.target;
  if (target instanceof HTMLTextAreaElement) return false;
  // `closest`, not `isContentEditable`: a child of an editable region counts too.
  if (
    target instanceof Element &&
    target.closest('[contenteditable]:not([contenteditable="false"])')
  ) {
    return false;
  }
  return true;
}

export function usePalette() {
  return { open, requested, openPalette, chord };
}
