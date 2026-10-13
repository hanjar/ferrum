// Open/closed state of the AI chat panel. Deliberately tiny and free of heavy imports so the
// action registry can toggle it without pulling the whole AI runtime (built lazily on first open).
import { ui } from '../uiState.svelte';

class AiChatHost {
  open = $state(false);
  /** Settings dialog should bring its AI section into view once (FER-429). */
  revealSettings = $state(false);
  /** Bumped when the provider setup changed: the open panel re-reads the new runtime (history is carried over). */
  runtimeVersion = $state(0);
  /** Element focused when the panel was opened; focus returns there on close. */
  returnFocus: HTMLElement | null = null;

  toggle(): void {
    if (this.open) {
      this.close();
      return;
    }
    const active = globalThis.document?.activeElement;
    this.returnFocus = active instanceof HTMLElement ? active : null;
    this.open = true;
  }

  close(): void {
    this.open = false;
  }

  /** Open Settings at the AI provider section (FER-412 setup lives in Settings since FER-429). */
  openSettings(): void {
    this.revealSettings = true;
    ui.settingsOpen = true;
  }
}

export const aiChatHost = new AiChatHost();
