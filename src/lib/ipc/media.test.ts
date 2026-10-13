import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invokeMock(...a),
  // Real shape on Windows: custom schemes become http://<scheme>.localhost/<path>.
  convertFileSrc: (p: string, scheme = 'asset') =>
    `http://${scheme}.localhost/${encodeURIComponent(p)}`,
}));

import { createGrantBatcher, mediaGrant, mediaRevoke, mediaUrl, newMediaScope } from './media';

const win = window as unknown as Record<string, unknown>;

beforeEach(() => {
  invokeMock.mockReset();
  win.__TAURI_INTERNALS__ = {};
});
afterEach(() => {
  delete win.__TAURI_INTERNALS__;
});

describe('ferrum-media client', () => {
  it('builds ferrum-media URLs from tokens only (never from a path)', () => {
    expect(mediaUrl('ab12')).toBe('http://ferrum-media.localhost/ab12');
  });

  it('maps tokens to URLs and keeps nulls for refused files', async () => {
    invokeMock.mockResolvedValue(['aa', null, 'bb']);
    const paths = ['C:\\a.png', 'C:\\x.html', 'C:\\b.png'];
    const urls = await mediaGrant(paths, 's');
    expect(invokeMock).toHaveBeenCalledWith('media_grant', { paths, scope: 's' });
    expect(urls).toEqual([
      'http://ferrum-media.localhost/aa',
      null,
      'http://ferrum-media.localhost/bb',
    ]);
  });

  it('outside Tauri nothing is granted and no IPC happens', async () => {
    delete win.__TAURI_INTERNALS__;
    expect(await mediaGrant(['C:\\a.png'], 's')).toEqual([null]);
    await mediaRevoke('s');
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('revoke swallows backend errors (grants also expire)', async () => {
    invokeMock.mockRejectedValue(new Error('boom'));
    await expect(mediaRevoke('s')).resolves.toBeUndefined();
    expect(invokeMock).toHaveBeenCalledWith('media_revoke', { scope: 's' });
  });

  it('scopes are unique per call', () => {
    expect(newMediaScope('grid')).not.toBe(newMediaScope('grid'));
  });

  it('the batcher coalesces same-tick requests into one grant call', async () => {
    invokeMock.mockResolvedValue(['t1', 't2']);
    const b = createGrantBatcher('grid:9');
    const [u1, u2] = await Promise.all([b.request('C:\\1.png'), b.request('C:\\2.png')]);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('media_grant', {
      paths: ['C:\\1.png', 'C:\\2.png'],
      scope: 'grid:9',
    });
    expect([u1, u2]).toEqual([
      'http://ferrum-media.localhost/t1',
      'http://ferrum-media.localhost/t2',
    ]);
  });

  it('the batcher resolves null when the grant call fails', async () => {
    invokeMock.mockRejectedValue(new Error('x'));
    const b = createGrantBatcher('grid:10');
    expect(await b.request('C:\\1.png')).toBeNull();
  });
});
