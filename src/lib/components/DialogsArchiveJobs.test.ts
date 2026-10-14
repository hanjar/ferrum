import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';

// FER-79 / FER-73 / FER-75: the dialogs for a running archive job, the password prompt and
// the multipart explanation. The IPC is mocked; only the UI contract is under test.
const ipc = vi.hoisted(() => ({
  cancel: vi.fn(),
}));
vi.mock('../ipc/archive', () => ({
  archiveList: vi.fn(),
  archiveRead: vi.fn(),
  archiveExtract: vi.fn(),
  archiveExtractPreflight: vi.fn(),
  archiveTest: vi.fn(),
  archiveCreate: vi.fn(),
  archiveCreateZip: vi.fn(),
  archiveCancelRequest: (...args: unknown[]) => ipc.cancel(...args),
  archiveMultipartCheck: vi.fn(),
  archiveRarToolProbe: vi.fn(async () => ({ ok: false })),
  archiveRarToolDetect: vi.fn(async () => null),
  newArchiveRequestId: () => 'req-x',
  listenArchiveProgress: async () => () => {},
}));

afterEach(() => {
  dialogs.current = null;
  ipc.cancel.mockReset();
});

function openProgress(over: Record<string, unknown> = {}) {
  dialogs.open({
    kind: 'archiveProgress',
    title: 'Unpack',
    archivePath: 'C:\\a\\big.zip',
    position: '',
    requestId: 'req-1',
    processedBytes: 5 * 1024 * 1024,
    totalBytes: 10 * 1024 * 1024,
    processedEntries: 3,
    totalEntries: 6,
    currentMember: 'docs/readme.md',
    cancelling: false,
    ...over,
  });
}

describe('archive progress dialog', () => {
  it('shows bytes, members and the current member with a progressbar', async () => {
    render(Dialogs);
    openProgress();
    await tick();
    const bar = screen.getByRole('progressbar');
    expect(bar.getAttribute('aria-valuenow')).toBe('50');
    expect(screen.getByText(/5\.0 MiB \/ 10\.0 MiB/)).toBeTruthy();
    expect(screen.getByText('docs/readme.md')).toBeTruthy();
  });

  it('the Batal button cancels the job by its request id', async () => {
    render(Dialogs);
    openProgress();
    await tick();
    await fireEvent.click(screen.getByRole('button', { name: 'Batal' }));
    expect(ipc.cancel).toHaveBeenCalledWith('req-1');
    await tick();
    expect(screen.getByText(/Membatalkan/)).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Batal' }) as HTMLButtonElement).disabled).toBe(
      true,
    );
  });

  it('Esc cancels the job instead of just hiding the dialog', async () => {
    render(Dialogs);
    openProgress();
    await tick();
    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(ipc.cancel).toHaveBeenCalledWith('req-1');
    expect(dialogs.current).toMatchObject({ kind: 'archiveProgress', cancelling: true });
  });

  it('clicking the backdrop does not cancel a running job', async () => {
    const { container } = render(Dialogs);
    openProgress();
    await tick();
    await fireEvent.mouseDown(container.querySelector('.scrim')!);
    expect(ipc.cancel).not.toHaveBeenCalled();
    expect(dialogs.current).toMatchObject({ kind: 'archiveProgress' });
  });

  it('without a known total the bar is indeterminate', async () => {
    render(Dialogs);
    openProgress({ totalBytes: undefined, totalEntries: undefined });
    await tick();
    expect(screen.getByRole('progressbar').getAttribute('aria-valuenow')).toBeNull();
  });
});

describe('password dialog', () => {
  it('is masked, shows the wrong-password message, and clears the field once submitted', async () => {
    const retry = vi.fn().mockResolvedValue(undefined);
    render(Dialogs);
    dialogs.open({
      kind: 'password',
      archivePath: 'C:\\a\\enc.zip',
      message: 'Password salah. Coba lagi.',
      retry,
    });
    await tick();
    expect(screen.getByRole('alert').textContent).toContain('Password salah');
    const input = screen.getByPlaceholderText('Archive password') as HTMLInputElement;
    expect(input.type).toBe('password');
    expect(input.getAttribute('autocomplete')).toBe('off');
    await fireEvent.input(input, { target: { value: 's3cret' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Unlock' }));
    expect(retry).toHaveBeenCalledWith('s3cret');
    await tick();
    expect(input.value).toBe('');
  });

  it('Esc settles the prompt through its cancel hook', async () => {
    const cancel = vi.fn();
    render(Dialogs);
    dialogs.open({
      kind: 'password',
      archivePath: 'C:\\a\\enc.zip',
      retry: async () => {},
      cancel,
    });
    await tick();
    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(cancel).toHaveBeenCalledOnce();
    expect(dialogs.current).toBeNull();
  });
});

describe('cancel is not shown as an error', () => {
  it('the cancelled-extract notice is a plain dialog, not a red alert', async () => {
    render(Dialogs);
    dialogs.open({
      kind: 'error',
      title: 'Unpack',
      message: 'Dibatalkan. Berkas yang sudah selesai diekstrak tetap ada.',
    });
    await tick();
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByText(/Dibatalkan/)).toBeTruthy();
  });

  it('a cancelled test is labelled Dibatalkan in the report, not Gagal', async () => {
    render(Dialogs);
    dialogs.open({
      kind: 'testReport',
      title: 'Test archive dibatalkan',
      reports: [
        {
          archivePath: 'x.zip',
          status: 'cancelled',
          passed: 0,
          total: 0,
          failures: [],
          note: 'Dibatalkan saat diuji.',
        },
      ],
    });
    await tick();
    expect(screen.getAllByText('Dibatalkan').length).toBeGreaterThan(0);
    expect(screen.queryByText(/Gagal/)).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('FER-508 (P3): sanitizes bidi, control characters, and line/paragraph separators in test report failure list', async () => {
    render(Dialogs);
    dialogs.open({
      kind: 'testReport',
      title: 'Hasil tes',
      reports: [
        {
          archivePath: 'corrupt.zip',
          status: 'failed',
          passed: 0,
          total: 1,
          failures: [
            {
              memberPath: 'evil\u202Edir/\u2028file\u2029\nname.txt',
              status: 'failed',
              message: 'CRC mismatch',
            },
          ],
        },
      ],
    });
    await tick();
    const sanitized = 'evil\\u{202E}dir/\\u{2028}file\\u{2029}\\nname.txt';
    expect(screen.getByText(sanitized)).toBeTruthy();
    expect(screen.queryByText(/\u202E/)).toBeNull();
    expect(screen.queryByText(/\u2028/)).toBeNull();
    expect(screen.queryByText(/\u2029/)).toBeNull();
  });
});

describe('multipart dialog', () => {
  it('part 2 says which part it is and offers to open the first one', async () => {
    render(Dialogs);
    dialogs.open({
      kind: 'multipart',
      archivePath: 'C:\\a\\x.part2.rar',
      info: {
        kind: 'notFirst',
        part: 2,
        firstName: 'x.part1.rar',
        firstPath: 'C:\\a\\x.part1.rar',
        firstExists: true,
      },
    });
    await tick();
    expect(screen.getByText(/Ini bagian 2 dari arsip multipart/)).toBeTruthy();
    expect(screen.getByText('x.part1.rar')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Buka bagian pertama' })).toBeTruthy();
  });

  it('a missing first part has no open button and says it is not in the folder', async () => {
    render(Dialogs);
    dialogs.open({
      kind: 'multipart',
      archivePath: 'C:\\a\\x.7z.002',
      info: {
        kind: 'notFirst',
        part: 2,
        firstName: 'x.7z.001',
        firstPath: 'C:\\a\\x.7z.001',
        firstExists: false,
      },
    });
    await tick();
    expect(screen.queryByRole('button', { name: 'Buka bagian pertama' })).toBeNull();
    expect(screen.getByText(/tidak ditemukan di folder/)).toBeTruthy();
  });

  it('a first part with gaps lists the missing volumes and refuses to continue', async () => {
    render(Dialogs);
    dialogs.open({
      kind: 'multipart',
      archivePath: 'C:\\a\\x.part1.rar',
      info: { kind: 'missing', missing: ['x.part2.rar', 'x.part4.rar'] },
    });
    await tick();
    expect(screen.getByText(/Bagian arsip multipart hilang/)).toBeTruthy();
    expect(screen.getByText('x.part2.rar')).toBeTruthy();
    expect(screen.getByText('x.part4.rar')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Buka bagian pertama' })).toBeNull();
  });
});

describe('FER-55: error per item and extract summary', () => {
  it('error dialog specifies the failed member path and reason', async () => {
    render(Dialogs);
    dialogs.open({
      kind: 'error',
      title: 'Extract',
      message: 'Gagal di: assets/photo.png\nExtraction write failed',
    });
    await tick();
    const modal = screen.getByRole('dialog');
    expect(modal.textContent).toContain('Gagal di: assets/photo.png');
    expect(modal.textContent).toContain('Extraction write failed');
  });

  it('extract summary dialog displays written, skipped, and renamed file counts', async () => {
    render(Dialogs);
    dialogs.showExtractSummary({
      written: 12,
      skipped: 3,
      renamed: 2,
      target: 'D:\\output',
    });
    await tick();
    const modal = screen.getByRole('dialog');
    expect(modal.textContent).toContain('Ekstrak selesai');
    expect(modal.textContent).toContain('Ditulis: 12 berkas');
    expect(modal.textContent).toContain('Dilewati: 3 berkas');
    expect(modal.textContent).toContain('Dinamai ulang: 2 berkas');
    expect(modal.textContent).toContain('D:\\output');

    await fireEvent.click(screen.getByRole('button', { name: 'Tutup' }));
    await tick();
    expect(dialogs.current).toBeNull();
  });

  it('archive progress dialog shows written and skipped entries when present', async () => {
    render(Dialogs);
    dialogs.open({
      kind: 'archiveProgress',
      title: 'Unpack',
      archivePath: 'C:\\a\\big.zip',
      position: '',
      requestId: 'req-summary',
      processedBytes: 10 * 1024 * 1024,
      totalBytes: 10 * 1024 * 1024,
      processedEntries: 10,
      totalEntries: 10,
      writtenEntries: 8,
      skippedEntries: 2,
      cancelling: false,
    });
    await tick();
    const modal = screen.getByRole('dialog');
    expect(modal.textContent).toContain('8 ditulis');
    expect(modal.textContent).toContain('2 dilewati');
  });
});
