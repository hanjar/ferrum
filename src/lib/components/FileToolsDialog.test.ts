import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/svelte';

const verifyMock = vi.fn();
const cancelMock = vi.fn();
vi.mock('../ipc/fileTools', async (importOriginal) => {
  const original = await importOriginal<typeof import('../ipc/fileTools')>();
  return {
    ...original,
    verifyChecksums: (...a: unknown[]) => verifyMock(...a),
    cancelChecksumRequest: (...a: unknown[]) => cancelMock(...a),
    enqueueChecksumCreate: vi.fn(),
    enqueueSplitFile: vi.fn(),
    enqueueCombineParts: vi.fn(),
  };
});

const { fileTools } = await import('../stores/fileToolsStore.svelte');
import FileToolsDialog from './FileToolsDialog.svelte';

afterEach(() => {
  cleanup();
  fileTools.close();
  verifyMock.mockReset();
  cancelMock.mockReset();
});

describe('FileToolsDialog', () => {
  it('renders nothing while closed and shows the create sheet with SHA-256 default', async () => {
    const { container } = render(FileToolsDialog);
    expect(container.querySelector('[role="dialog"]')).toBeNull();
    fileTools.open({ kind: 'checksumCreate', paths: ['C:\\d\\a.txt', 'C:\\d\\b.txt'] });
    await vi.waitFor(() =>
      expect(screen.getByRole('dialog', { name: 'Create Checksum File(s)' })).toBeTruthy(),
    );
    const sha = screen.getByRole('radio', { name: 'SHA-256' }) as HTMLInputElement;
    expect(sha.checked).toBe(true);
    expect(screen.getByText('a.txt')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Create' })).toBeTruthy();
  });

  it('shows verification results with OK and MISMATCH counts', async () => {
    verifyMock.mockResolvedValue({
      requested: 2,
      ok: 1,
      mismatched: 1,
      outcomes: [
        { path: 'C:\\d\\good.txt', status: 'ok' },
        {
          path: 'C:\\d\\bad.txt',
          status: 'mismatch',
          expected: 'aa',
          actual: 'bb',
        },
        { path: 'C:\\d\\ghost.txt', status: 'missing' },
      ],
    });
    fileTools.open({ kind: 'checksumVerify', paths: ['C:\\d\\good.txt'] });
    render(FileToolsDialog);
    await vi.waitFor(() => expect(screen.getByText('MISMATCH')).toBeTruthy());
    expect(screen.getByText('OK')).toBeTruthy();
    expect(screen.getByText('missing')).toBeTruthy();
    expect(screen.getByText('bad.txt')).toBeTruthy();
  });

  it('labels legacy algorithms honestly in the create sheet', () => {
    fileTools.open({ kind: 'checksumCreate', paths: ['C:\\d\\a.txt'] });
    render(FileToolsDialog);
    expect(screen.getByRole('radio', { name: 'SHA-1 (legacy verify)' })).toBeTruthy();
    expect(screen.getByRole('radio', { name: 'MD5 (legacy verify)' })).toBeTruthy();
    expect(screen.getByRole('radio', { name: 'CRC32 (legacy verify)' })).toBeTruthy();
  });

  it('offers a conflict policy that defaults to refuse and only renames combined files', async () => {
    fileTools.open({ kind: 'checksumCreate', paths: ['C:\\d\\a.txt'] });
    render(FileToolsDialog);
    const select = (await screen.findByLabelText(
      'If the output already exists',
    )) as HTMLSelectElement;
    expect(select.value).toBe('refuse');
    expect([...select.options].map((o) => o.value)).toEqual([
      'refuse',
      'skip',
      'overwrite',
      'overwriteOlder',
    ]);
    cleanup();
    fileTools.open({ kind: 'combine', firstPart: 'C:\\d\\f.iso.001' });
    render(FileToolsDialog);
    const combine = (await screen.findByLabelText(
      'If the output already exists',
    )) as HTMLSelectElement;
    expect([...combine.options].map((o) => o.value)).toContain('rename');
  });

  it('lets a running verification be cancelled and says the list is partial', async () => {
    let finish: (v: unknown) => void = () => {};
    verifyMock.mockImplementation(() => new Promise((resolve) => (finish = resolve)));
    cancelMock.mockResolvedValue(true);
    fileTools.open({ kind: 'checksumVerify', paths: ['C:\\d\\big.iso'] });
    render(FileToolsDialog);
    const cancel = await screen.findByRole('button', { name: 'Cancel' });
    cancel.click();
    await vi.waitFor(() => expect(cancelMock).toHaveBeenCalled());
    finish({ requested: 1, ok: 0, mismatched: 0, outcomes: [], cancelled: true });
    await vi.waitFor(() =>
      expect(screen.getByText(/Cancelled — only the items finished/)).toBeTruthy(),
    );
  });
});
