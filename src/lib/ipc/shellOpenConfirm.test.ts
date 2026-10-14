import { describe, expect, it, vi, beforeEach } from 'vitest';
import { FsError } from './types';
import { handleShellError, mapShellError } from './shellOpen';
import { toast } from '../stores/toastStore.svelte';
import { dialogs } from '../stores/dialogStore.svelte';

describe('FER-513: shell open UI error mapping', () => {
  beforeEach(() => {
    toast.message = '';
    dialogs.close();
  });

  describe('mapShellError pure mapping', () => {
    it('maps cancelled error to silent outcome', () => {
      const err = new FsError({ code: 'cancelled', message: 'Operasi dibatalkan oleh pengguna' });
      const mapped = mapShellError(err);
      expect(mapped).toEqual({ kind: 'silent' });
    });

    it('maps busy error to neutral toast outcome', () => {
      const err = new FsError({ code: 'busy', message: 'Konfirmasi lain masih terbuka' });
      const mapped = mapShellError(err);
      expect(mapped).toEqual({
        kind: 'toast',
        message: 'Konfirmasi lain masih terbuka',
      });
    });

    it('maps ordinary FsError to error dialog outcome', () => {
      const err = new FsError({ code: 'notFound', message: 'File or path not found' });
      const mapped = mapShellError(err);
      expect(mapped).toEqual({
        kind: 'error',
        message: 'File or path not found',
      });
    });

    it('maps unsupported error to error dialog outcome', () => {
      const err = new FsError({ code: 'unsupported', message: 'Action not supported' });
      const mapped = mapShellError(err);
      expect(mapped).toEqual({
        kind: 'error',
        message: 'Action not supported',
      });
    });

    it('maps generic JS Error to error dialog outcome', () => {
      const err = new Error('Unexpected platform crash');
      const mapped = mapShellError(err);
      expect(mapped).toEqual({
        kind: 'error',
        message: 'Unexpected platform crash',
      });
    });
  });

  describe('handleShellError integration with UI stores', () => {
    it('cancelled does not open error dialog and does not show toast', () => {
      const err = new FsError({ code: 'cancelled', message: 'Cancelled by user' });
      handleShellError('Open', err);
      expect(dialogs.current).toBeNull();
      expect(toast.message).toBe('');
    });

    it('busy displays toast and does not open error dialog', () => {
      const toastSpy = vi.spyOn(toast, 'show');
      const err = new FsError({ code: 'busy', message: 'Busy' });
      handleShellError('Open', err);
      expect(toastSpy).toHaveBeenCalledWith('Konfirmasi lain masih terbuka');
      expect(dialogs.current).toBeNull();
      toastSpy.mockRestore();
    });

    it('other error opens error dialog with title and message', () => {
      const err = new FsError({ code: 'permissionDenied', message: 'Access denied' });
      handleShellError('Open', err);
      expect(dialogs.current).toMatchObject({
        kind: 'error',
        title: 'Open',
        message: 'Access denied',
      });
    });
  });
});
