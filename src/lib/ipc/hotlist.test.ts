import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

const { asView, defaultBackend, HotlistError, resetMemoryBackend } = await import('./hotlist');

function fakeTauri() {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}
function unTauri() {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
}

const VIEW = {
  doc: { version: 2, items: [{ type: 'dir', title: 'A', path: 'C:\\a' }] },
  status: 'ok',
  readOnly: false,
  customLocation: 'D:\\h.json',
};

beforeEach(() => {
  invokeMock.mockReset().mockName('invoke');
  unTauri();
  resetMemoryBackend();
});

describe('hotlist IPC wrappers', () => {
  it('maps the backend reply and keeps verbatim arguments', async () => {
    fakeTauri();
    const backend = defaultBackend();
    invokeMock.mockResolvedValueOnce(VIEW);
    expect(await backend.load()).toEqual(VIEW);
    expect(invokeMock).toHaveBeenLastCalledWith('hotlist_load', undefined);

    const doc = VIEW.doc as never;
    invokeMock.mockResolvedValueOnce(VIEW);
    await backend.save(doc);
    expect(invokeMock).toHaveBeenLastCalledWith('hotlist_save', { doc });

    invokeMock.mockResolvedValueOnce(true);
    expect(await backend.checkLocation('D:\\x y\\h.json')).toBe(true);
    expect(invokeMock).toHaveBeenLastCalledWith('hotlist_check_location', {
      path: 'D:\\x y\\h.json',
    });

    invokeMock.mockResolvedValueOnce(VIEW);
    await backend.setLocation(null, 'use');
    expect(invokeMock).toHaveBeenLastCalledWith('hotlist_set_location', {
      path: null,
      mode: 'use',
    });
  });

  it('sends the place in the tree and the shown item, never a "confirmed" flag', async () => {
    fakeTauri();
    invokeMock.mockResolvedValueOnce({ launched: false });
    const item = {
      type: 'command' as const,
      title: '&k',
      command: 'C:\\a b\\x.exe',
      args: ['--p', 'q r'],
    };
    const r = await defaultBackend().runCommand([1, 0], item);
    expect(r).toEqual({ launched: false });
    expect(invokeMock).toHaveBeenLastCalledWith('hotlist_run_command', {
      position: [1, 0],
      title: '&k',
      command: 'C:\\a b\\x.exe',
      args: ['--p', 'q r'],
    });
    // FER-467 R5: the webview has no way to say "already confirmed".
    expect(JSON.stringify(invokeMock.mock.lastCall)).not.toMatch(/confirm/i);
    invokeMock.mockResolvedValueOnce({ launched: true });
    expect(await defaultBackend().runCommand([1, 0], item)).toEqual({ launched: true });
  });

  it('keeps the unsafe status and the problem of a refused custom location', () => {
    expect(
      asView({ ...VIEW, status: 'unsafe', readOnly: true, locationProblem: 'location_link' }),
    ).toEqual({ ...VIEW, status: 'unsafe', readOnly: true, locationProblem: 'location_link' });
  });

  it('turns backend errors into safe kinds', async () => {
    fakeTauri();
    invokeMock.mockRejectedValueOnce({ kind: 'conflict' });
    await expect(defaultBackend().save(VIEW.doc as never)).rejects.toMatchObject({
      kind: 'conflict',
    });
    invokeMock.mockRejectedValueOnce('boom');
    await expect(defaultBackend().load()).rejects.toBeInstanceOf(HotlistError);
    invokeMock.mockRejectedValueOnce('boom');
    await expect(defaultBackend().importPreview(null)).rejects.toMatchObject({ kind: 'unknown' });
  });

  it('validates the import preview reply', async () => {
    fakeTauri();
    invokeMock.mockResolvedValueOnce({ doc: { version: 7, items: [] }, warnings: [] });
    await expect(defaultBackend().importPreview(null)).rejects.toMatchObject({
      kind: 'invalid_doc',
    });
    invokeMock.mockResolvedValueOnce({
      doc: { version: 2, items: [] },
      warnings: [{ index: 3, kind: 'no_action' }],
    });
    expect(await defaultBackend().importPreview('C:\\w.ini')).toEqual({
      doc: { version: 2, items: [] },
      warnings: [{ index: 3, kind: 'no_action' }],
    });
  });

  it('degrades a malformed reply to an empty writable list (older mock backends)', () => {
    for (const bad of [null, undefined, 'x', { doc: null }, { ...VIEW, status: 'weird' }]) {
      expect(asView(bad)).toEqual({
        doc: { version: 2, items: [] },
        status: 'new',
        readOnly: false,
        customLocation: null,
      });
    }
  });
});

describe('browser-dev fallback', () => {
  it('keeps the list in memory and refuses file and launch features', async () => {
    const backend = defaultBackend();
    expect((await backend.load()).doc.items).toEqual([]);
    await backend.save(VIEW.doc as never);
    expect((await backend.load()).doc.items).toHaveLength(1);
    await expect(
      backend.runCommand([0], { type: 'command', title: 'x', command: 'x', args: [] }),
    ).rejects.toMatchObject({ kind: 'unsupported' });
    await expect(backend.importPreview(null)).rejects.toMatchObject({ kind: 'unsupported' });
    await expect(backend.checkLocation('D:\\h.json')).rejects.toMatchObject({
      kind: 'unsupported',
    });
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
