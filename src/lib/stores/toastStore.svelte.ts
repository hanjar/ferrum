/** FER-483: tiny transient message ("Disalin: 3 nama"). One message at a time. */
class ToastStore {
  message = $state('');
  #timer: ReturnType<typeof setTimeout> | null = null;

  show(message: string, ms = 2200): void {
    this.message = message;
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = setTimeout(() => {
      this.message = '';
      this.#timer = null;
    }, ms);
  }
}

export const toast = new ToastStore();
