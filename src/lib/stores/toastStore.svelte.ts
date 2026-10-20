/** FER-483: tiny transient message ("Disalin: 3 nama"). One message at a time. */
export interface ToastAction {
  label: string;
  run: () => void;
}

class ToastStore {
  message = $state('');
  /** FER-578: optional button ("Buka") for a message that points at something to open. */
  action = $state<ToastAction | null>(null);
  #timer: ReturnType<typeof setTimeout> | null = null;

  show(message: string, ms = 2200, action?: ToastAction): void {
    this.message = message;
    this.action = message ? (action ?? null) : null;
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = setTimeout(() => {
      this.message = '';
      this.action = null;
      this.#timer = null;
    }, ms);
  }

  runAction(): void {
    const a = this.action;
    this.message = '';
    this.action = null;
    a?.run();
  }
}

export const toast = new ToastStore();
