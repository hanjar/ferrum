import { afterEach, describe, expect, it, vi } from 'vitest';
import { toast } from './toastStore.svelte';

afterEach(() => {
  toast.show('', 0);
  vi.useRealTimers();
});

describe('toast', () => {
  it('shows a message and clears it after the timeout', () => {
    vi.useFakeTimers();
    toast.show('Halo', 1000);
    expect(toast.message).toBe('Halo');
    expect(toast.action).toBeNull();
    vi.advanceTimersByTime(1000);
    expect(toast.message).toBe('');
  });

  it('keeps an optional action and runs it once, closing the toast', () => {
    const run = vi.fn();
    toast.show('Password diperlukan', 5000, { label: 'Buka', run });
    expect(toast.action?.label).toBe('Buka');
    toast.runAction();
    expect(run).toHaveBeenCalledTimes(1);
    expect(toast.message).toBe('');
    toast.runAction();
    expect(run).toHaveBeenCalledTimes(1);
  });

  it('a later plain message drops the previous action', () => {
    toast.show('a', 5000, { label: 'Buka', run: () => {} });
    toast.show('b');
    expect(toast.action).toBeNull();
  });
});
