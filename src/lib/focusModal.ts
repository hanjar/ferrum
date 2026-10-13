/** Svelte action for dialogs whose keyboard handling (Escape, Tab) lives on
 *  the modal element itself: move focus there on open — otherwise it stays
 *  on the file row behind the scrim and the dialog's `onkeydown` never fires —
 *  and hand it back to the opener when the dialog goes away. */
export function focusModal(node: HTMLElement) {
  const opener = document.activeElement as HTMLElement | null;
  node.focus();
  return {
    destroy() {
      opener?.focus?.();
    },
  };
}
