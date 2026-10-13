import { afterEach, describe, expect, it, vi } from 'vitest';
import { copyText } from './clipboard';

afterEach(() => {
  vi.unstubAllGlobals();
});

function fakeDocument(execCommand: unknown): Document {
  const textarea = {
    value: '',
    style: {},
    setAttribute: vi.fn(),
    select: vi.fn(),
    remove: vi.fn(),
  };
  return {
    createElement: vi.fn(() => textarea),
    body: { appendChild: vi.fn() },
    execCommand,
  } as unknown as Document;
}

describe('copyText', () => {
  it('uses the async clipboard API when available', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal('navigator', { clipboard: { writeText } });
    await expect(copyText('hello')).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith('hello');
  });

  it('falls back to a hidden textarea + execCommand when the API is missing', async () => {
    vi.stubGlobal('navigator', {});
    const doc = fakeDocument(vi.fn().mockReturnValue(true));
    vi.stubGlobal('document', doc);
    await expect(copyText('hello')).resolves.toBe(true);
    expect(doc.createElement).toHaveBeenCalledWith('textarea');
    expect(doc.execCommand).toHaveBeenCalledWith('copy');
  });

  it('reports failure when every path fails', async () => {
    vi.stubGlobal('navigator', {});
    vi.stubGlobal('document', fakeDocument(vi.fn().mockReturnValue(false)));
    await expect(copyText('hello')).resolves.toBe(false);
  });

  it('falls back when writeText rejects (permission denied)', async () => {
    const writeText = vi.fn().mockRejectedValue(new Error('denied'));
    vi.stubGlobal('navigator', { clipboard: { writeText } });
    vi.stubGlobal('document', fakeDocument(vi.fn().mockReturnValue(true)));
    await expect(copyText('x')).resolves.toBe(true); // legacy path rescues it
    expect(writeText).toHaveBeenCalledOnce();
  });
});
