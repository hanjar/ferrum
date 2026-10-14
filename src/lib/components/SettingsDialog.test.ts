import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import SettingsDialog from './SettingsDialog.svelte';
import { ui, TABS_DEFAULTS, PREVIEW_DEFAULTS } from '../uiState.svelte';
import { DEFAULT_FILE_OPERATION_PREFS } from '../ipc/prefs';
import { prefs } from '../stores/prefsStore.svelte';

afterEach(() => {
  cleanup();
  ui.tabs = { ...TABS_DEFAULTS };
  ui.preview = { ...PREVIEW_DEFAULTS };
  Object.assign(prefs.fileOperations, DEFAULT_FILE_OPERATION_PREFS);
});

function renderDialog() {
  const onclose = vi.fn();
  const host = render(SettingsDialog, { onclose });
  const checkbox = (label: string) => host.getByLabelText(label) as HTMLInputElement;
  const radio = (label: string) => host.getByRole('radio', { name: label });
  return { host, onclose, checkbox, radio };
}

describe('SettingsDialog (FER-395 Tabs section)', () => {
  it('renders the Tabs controls with the screenshot-spec defaults and a Fit/Fixed group', () => {
    const { checkbox, radio } = renderDialog();
    // Defaults from the screenshot spec: all three toggles on, Fit selected.
    expect(checkbox('Open new tabs next to current').checked).toBe(true);
    expect(checkbox('Restore tabs on launch').checked).toBe(true);
    expect(checkbox('Close tab with middle click').checked).toBe(true);
    expect(radio('Fit').getAttribute('aria-checked')).toBe('true');
    expect(radio('Fixed').getAttribute('aria-checked')).toBe('false');
  });

  it('checkboxes write through to ui.tabs', () => {
    const { checkbox } = renderDialog();
    fireEvent.click(checkbox('Open new tabs next to current'));
    fireEvent.click(checkbox('Restore tabs on launch'));
    fireEvent.click(checkbox('Close tab with middle click'));
    expect(ui.tabs.openNextToCurrent).toBe(false);
    expect(ui.tabs.restoreTabs).toBe(false);
    expect(ui.tabs.closeOnMiddleClick).toBe(false);
  });

  it('the Fit/Fixed segmented group writes ui.tabs.tabWidth', () => {
    const { radio } = renderDialog();
    fireEvent.click(radio('Fixed'));
    expect(ui.tabs.tabWidth).toBe('fixed');
    expect(radio('Fixed').getAttribute('aria-checked')).toBe('true');
    fireEvent.click(radio('Fit'));
    expect(ui.tabs.tabWidth).toBe('fit');
  });

  it('reflects pre-set ui state and still closes via Escape', async () => {
    ui.tabs = {
      openNextToCurrent: false,
      restoreTabs: true,
      tabWidth: 'fixed',
      closeOnMiddleClick: true,
    };
    const { checkbox, radio, onclose } = renderDialog();
    expect(checkbox('Open new tabs next to current').checked).toBe(false);
    expect(radio('Fixed').getAttribute('aria-checked')).toBe('true');

    const dialog = document.querySelector('[role="dialog"]') as HTMLElement;
    await fireEvent.keyDown(dialog, { key: 'Escape' });
    expect(onclose).toHaveBeenCalledOnce();
  });
});

describe('SettingsDialog (FER-396 Preview section)', () => {
  it('shows the Preview category and heading', () => {
    const { host } = renderDialog();
    const cat = host.getByTitle('Preview') as HTMLButtonElement;
    expect(cat.disabled).toBe(false);
    expect(host.getByText('What the preview panel renders')).toBeTruthy();
  });

  it('renders the five controls with the contract defaults', () => {
    const { checkbox, radio } = renderDialog();
    expect(checkbox('Open preview panel on launch').checked).toBe(false);
    expect(checkbox('Syntax highlighting').checked).toBe(true);
    expect(checkbox('Autoplay video').checked).toBe(false);
    expect(checkbox('Read EXIF & media metadata').checked).toBe(true);
    for (const l of ['100 KB', '500 KB', '1 MB', '2 MB', '4 MB']) {
      expect(radio(l).getAttribute('aria-checked')).toBe(l === '2 MB' ? 'true' : 'false');
    }
  });

  it('switches write through to ui.preview', () => {
    const { checkbox } = renderDialog();
    fireEvent.click(checkbox('Open preview panel on launch'));
    fireEvent.click(checkbox('Syntax highlighting'));
    fireEvent.click(checkbox('Autoplay video'));
    fireEvent.click(checkbox('Read EXIF & media metadata'));
    expect(ui.preview).toEqual({
      ...PREVIEW_DEFAULTS,
      openOnLaunch: true,
      syntaxHighlight: false,
      autoplayVideo: true,
      readMetadata: false,
    });
  });

  it('the limit group writes ui.preview.maxTextBytes by click and arrow keys', async () => {
    const { radio } = renderDialog();
    fireEvent.click(radio('500 KB'));
    expect(ui.preview.maxTextBytes).toBe(500_000);
    expect(radio('500 KB').getAttribute('aria-checked')).toBe('true');

    const group = document.querySelector('[aria-label="Max text preview"]') as HTMLElement;
    await fireEvent.keyDown(group, { key: 'ArrowRight' });
    expect(ui.preview.maxTextBytes).toBe(1_000_000);
    await fireEvent.keyDown(group, { key: 'End' });
    expect(ui.preview.maxTextBytes).toBe(4_000_000);
    await fireEvent.keyDown(group, { key: 'ArrowRight' }); // wraps
    expect(ui.preview.maxTextBytes).toBe(100_000);
    await fireEvent.keyDown(group, { key: 'Home' });
    expect(ui.preview.maxTextBytes).toBe(100_000);
  });
});

describe('SettingsDialog file operations', () => {
  it('renders and updates persisted file-operation controls', async () => {
    render(SettingsDialog);
    const confirm = screen.getByRole('switch', { name: 'Confirm before deleting' });
    await fireEvent.click(confirm);
    expect(prefs.fileOperations.confirmBeforeDeleting).toBe(false);
    expect(screen.getByText(/Permanent deletion always asks for confirmation/)).toBeTruthy();

    await fireEvent.click(screen.getByRole('switch', { name: 'Verify after copy' }));
    expect(prefs.fileOperations.verifyAfterCopy).toBe(true);
    await fireEvent.change(screen.getByRole('combobox', { name: 'When a file already exists' }), {
      target: { value: 'skip' },
    });
    expect(prefs.fileOperations.conflictPolicy).toBe('skip');
    await fireEvent.click(screen.getByRole('switch', { name: 'Preserve timestamps' }));
    expect(prefs.fileOperations.preserveTimestamps).toBe(false);
  });

  it('FER-471: default copy threads and priority are editable', async () => {
    render(SettingsDialog);
    const threads = screen.getByRole('combobox', { name: 'Thread copy' }) as HTMLSelectElement;
    expect(threads.value).toBe('0');
    expect(Array.from(threads.options).map((o) => o.textContent?.trim())).toEqual([
      'Otomatis',
      '1',
      '2',
      '4',
      '8',
      '16',
    ]);
    await fireEvent.change(threads, { target: { value: '8' } });
    expect(prefs.fileOperations.copyThreads).toBe(8);
    await fireEvent.change(screen.getByRole('combobox', { name: 'Prioritas copy' }), {
      target: { value: 'background' },
    });
    expect(prefs.fileOperations.copyPriority).toBe('background');
    const safe = screen.getByRole('switch', { name: 'Tulis aman (flush) untuk copy' });
    expect((safe as HTMLInputElement).checked).toBe(false);
    await fireEvent.click(safe);
    expect(prefs.fileOperations.copySafeWrite).toBe(true);
  });

  it('clamps copy buffer input to supported range', async () => {
    render(SettingsDialog);
    const buffer = screen.getByRole('spinbutton', { name: /Copy buffer/ });
    await fireEvent.change(buffer, { target: { value: '20000' } });
    expect(prefs.fileOperations.copyBufferKiB).toBe(8192);
  });
});

describe('SettingsDialog category nav (FER-429)', () => {
  it('highlights one category at a time and every active category is clickable', async () => {
    const { host } = renderDialog();
    const nav = host.container.querySelector('.cats nav')!;
    const current = () =>
      Array.from(nav.querySelectorAll('[aria-current="true"]')).map((b) => b.textContent?.trim());
    expect(current()).toEqual(['Appearance']);
    expect(nav.querySelectorAll('.cat.sel')).toHaveLength(1);
    for (const label of ['Tabs', 'File Operations', 'Preview', 'AI', 'Appearance']) {
      const button = host.getByRole('button', { name: label }) as HTMLButtonElement;
      expect(button.disabled).toBe(false);
      expect(host.container.querySelector(`[data-cat="${label}"]`)).not.toBeNull();
      await fireEvent.click(button);
      expect(current()).toEqual([label]);
      expect(host.container.querySelector('.pTitle h3')?.textContent).toBe(label);
    }
    for (const label of ['Keyboard Shortcuts', 'Network', 'Advanced']) {
      expect((host.getByRole('button', { name: label }) as HTMLButtonElement).disabled).toBe(true);
    }
  });
});

describe('SettingsDialog build info (FER-548)', () => {
  it('displays full build label and detailed tooltip in sidebar footer', () => {
    const { host } = renderDialog();
    const label = host.container.querySelector('.buildLabelText') as HTMLElement;
    expect(label).not.toBeNull();
    expect(label.textContent?.trim()).toBe('Ferrum · 0.2.0 · test000 · 14 Okt');
    expect(label.getAttribute('title')).toContain('Ferrum 0.2.0');
    expect(label.getAttribute('title')).toContain('Commit: test000');
  });

  it('copies build detail to clipboard when "Salin info build" button is clicked', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(globalThis.navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });

    const { host } = renderDialog();
    const btn = host.getByRole('button', { name: /Salin info build/i });
    expect(btn.getAttribute('title')).toContain('Ferrum 0.2.0');
    await fireEvent.click(btn);

    expect(writeText).toHaveBeenCalledTimes(1);
    const copiedText = writeText.mock.calls[0][0];
    expect(copiedText).toContain('Ferrum 0.2.0');
    expect(copiedText).toContain('Commit: test000');
    expect(copiedText).toContain('Status: clean');
    expect(copiedText).toContain('Built: 2026-10-14 00:00:00 UTC');
  });
});
