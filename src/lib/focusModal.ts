import { dialog } from './dialogKit';

/** Svelte action for modals whose Escape/Enter handling lives on the modal element itself.
 *  FER-577: now a thin alias of `use:dialog` (dialogKit.ts): focus moves into the modal on
 *  open, Tab is trapped, focus returns to the opener on destroy, and the modal counts toward
 *  `modalDepth()` so the global shortcuts stay quiet behind it. */
export function focusModal(node: HTMLElement) {
  return dialog(node);
}
