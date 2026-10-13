import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/svelte';

const memberMock = vi.fn();
vi.mock('../ipc/fileTools', async (importOriginal) => {
  const original = await importOriginal<typeof import('../ipc/fileTools')>();
  return {
    ...original,
    checksumArchiveMember: (...a: unknown[]) => memberMock(...a),
  };
});

const { fileTools } = await import('../stores/fileToolsStore.svelte');
import FileToolsDialog from './FileToolsDialog.svelte';

afterEach(() => {
  cleanup();
  fileTools.close();
  memberMock.mockReset();
});

describe('FileToolsDialog: archive member checksum (FER-133)', () => {
  it('computes the checksum from the dialog and shows the hash', async () => {
    memberMock.mockResolvedValue({
      archivePath: 'C:\\d\\pack.zip',
      memberPath: 'docs/a.txt',
      algorithm: 'sha256',
      hash: 'deadbeef',
    });
    fileTools.open({
      kind: 'archiveChecksum',
      archivePath: 'C:\\d\\pack.zip',
      memberPath: 'docs/a.txt',
    });
    render(FileToolsDialog);
    expect(await screen.findByRole('dialog', { name: 'Checksum of Archive Member' })).toBeTruthy();
    (screen.getByRole('button', { name: 'Compute' }) as HTMLButtonElement).click();
    await vi.waitFor(() => expect(screen.getByTestId('member-hash').textContent).toBe('deadbeef'));
  });

  it('says RAR members are unsupported and disables Compute', async () => {
    fileTools.open({
      kind: 'archiveChecksum',
      archivePath: 'C:\\d\\pack.rar',
      memberPath: 'a.txt',
    });
    render(FileToolsDialog);
    expect(await screen.findByText(/RAR members cannot be hashed in place/)).toBeTruthy();
    const compute = screen.getByRole('button', { name: 'Compute' }) as HTMLButtonElement;
    expect(compute.disabled).toBe(true);
    expect(memberMock).not.toHaveBeenCalled();
  });

  it('closes with Escape while the modal holds focus (focusModal)', async () => {
    fileTools.open({
      kind: 'archiveChecksum',
      archivePath: 'C:\\d\\pack.zip',
      memberPath: 'a.txt',
    });
    render(FileToolsDialog);
    await screen.findByRole('dialog', { name: 'Checksum of Archive Member' });
    await vi.waitFor(() => expect(document.activeElement?.closest('[role="dialog"]')).toBeTruthy());
    (document.activeElement as HTMLElement).dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }),
    );
    await vi.waitFor(() => expect(fileTools.current).toBeNull());
  });
});
