// The run confirmation itself is a native Windows dialog built by the backend (FER-467 R5; see
// `src-tauri/src/hotlist/mod.rs` tests). This file keeps the Configure-side explanations.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import HotlistDialog from './HotlistDialog.svelte';
import { hotlist } from '../stores/hotlistStore.svelte';
import { resetMemoryBackend } from '../ipc/hotlist';

afterEach(async () => {
  cleanup();
  hotlist.cancelNetwork();
  hotlist.closeConfigure();
  hotlist.closeMenu();
  resetMemoryBackend();
  await hotlist.load();
  vi.restoreAllMocks();
});

describe('Configure explains refused commands', () => {
  async function openWithCommand(command: string, args: string[]) {
    render(HotlistDialog);
    await hotlist.replace([{ type: 'command', title: 'c', command: 'C:\\ok.exe', args: [] }]);
    hotlist.openConfigure();
    await screen.findByRole('dialog', { name: 'Configure Directory Hotlist' });
    await fireEvent.input(await screen.findByLabelText('Command'), { target: { value: command } });
    await fireEvent.input(screen.getByLabelText('Arguments'), {
      target: { value: args.join(' ') },
    });
  }

  it('blocks OK and says why for a URI, a network path and unsafe batch arguments', async () => {
    await openWithCommand('https://example.com/x.exe', []);
    expect(screen.getByTestId('command-problem').textContent).toMatch(/Web addresses/);
    expect(screen.getByRole('button', { name: 'OK' }).hasAttribute('disabled')).toBe(true);
    await fireEvent.input(screen.getByLabelText('Command'), {
      target: { value: '\\\\host@SSL\\share\\x.exe' },
    });
    expect(screen.getByTestId('command-problem').textContent).toMatch(/Network paths/);
    await fireEvent.input(screen.getByLabelText('Command'), { target: { value: 'C:\\r.bat' } });
    await fireEvent.input(screen.getByLabelText('Arguments'), { target: { value: 'a&b' } });
    expect(screen.getByTestId('command-problem').textContent).toMatch(/cmd\.exe/);
    expect(screen.getByRole('button', { name: 'OK' }).hasAttribute('disabled')).toBe(true);
  });

  it('shows a warning (not an error) for scripts and shortcuts', async () => {
    await openWithCommand('C:\\s\\go.ps1', []);
    expect(screen.queryByTestId('command-problem')).toBeNull();
    expect(screen.getByTestId('command-risk').textContent).toMatch(/script/);
    expect(screen.getByRole('button', { name: 'OK' }).hasAttribute('disabled')).toBe(false);
  });
});

describe('Configure refuses device folders (FER-467 H1)', () => {
  it.each(['\\\\.\\UNC\\evil\\share', '\\\\?\\GLOBALROOT\\Device\\Mup\\evil\\share'])(
    'explains and blocks OK for %s',
    async (bad) => {
      render(HotlistDialog);
      await hotlist.replace([{ type: 'dir', title: 'd', path: 'C:\\ok' }]);
      hotlist.openConfigure();
      await screen.findByRole('dialog', { name: 'Configure Directory Hotlist' });
      await fireEvent.input(await screen.findByLabelText('Path'), { target: { value: bad } });
      expect(screen.getByTestId('path-problem').textContent).toMatch(/Device paths/);
      expect(screen.getByRole('button', { name: 'OK' }).hasAttribute('disabled')).toBe(true);
      await fireEvent.input(screen.getByLabelText('Path'), { target: { value: 'C:\\fine' } });
      expect(screen.queryByTestId('path-problem')).toBeNull();
    },
  );
});
