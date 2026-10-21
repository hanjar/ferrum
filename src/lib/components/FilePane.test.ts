import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import FilePane from './FilePane.svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { endInternalDrag } from '../stores/dragPayload';
import { FsError } from '../ipc/types';
import { activity } from '../stores/activityStore.svelte';
import { getPane } from '../stores/panesRegistry.svelte';
import { ContextService } from '../ai/context';
import { registryHost } from '../ai/contextPanes';

// planCopy serves the dialog store; enqueueCopy is the mocked "enqueue path"
// the confirm button hits (browser mode → rejects 'unsupported').
const planCopyMock = vi.fn();
const enqueueCopyMock = vi.fn();
vi.mock('../ipc/mutations', () => ({
  planCopy: (...args: unknown[]) => planCopyMock(...args),
  enqueueCopy: (...args: unknown[]) => enqueueCopyMock(...args),
  enqueueMove: vi.fn(),
  createEntry: vi.fn(),
  renameEntry: vi.fn(),
  deleteEntries: vi.fn(),
}));

/** Map-backed DataTransfer stand-in (jsdom has no DataTransfer/DragEvent). */
function fakeDataTransfer(): DataTransfer {
  const store = new Map<string, string>();
  return {
    effectAllowed: 'all',
    dropEffect: 'none',
    get types(): string[] {
      return [...store.keys()];
    },
    setData(type: string, value: string): void {
      store.set(type, value);
    },
    getData(type: string): string {
      return store.get(type) ?? '';
    },
  } as unknown as DataTransfer;
}

/** Bubbling drag event carrying the stubbed dataTransfer (and shiftKey,
 *  which the Event constructor would otherwise drop). */
function dragEvent(type: string, dt: DataTransfer, shift = false): DragEvent {
  const e = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(e, 'dataTransfer', { value: dt });
  Object.defineProperty(e, 'shiftKey', { value: shift });
  return e as unknown as DragEvent;
}

const shellOpenMock = vi.fn();
vi.mock('../ipc/activity', async (orig) => ({
  ...(await orig<typeof import('../ipc/activity')>()),
  shellOpen: (...args: unknown[]) => shellOpenMock(...args),
}));

interface Mounted {
  container: HTMLElement;
}

let dialogHost: Mounted;
let left: Mounted;
let right: Mounted;

async function boot() {
  dialogHost = render(Dialogs);
  left = render(FilePane, { paneId: 0 });
  right = render(FilePane, { paneId: 1 });
  await vi.waitFor(() =>
    expect(left.container.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
  );
  await vi.waitFor(() =>
    expect(right.container.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
  );
}

const sectionOf = (m: Mounted) => m.container.querySelector('section') as HTMLElement;
const overlayOf = (m: Mounted) => m.container.querySelector('.dropOverlay');
const dialogText = () =>
  dialogHost.container.querySelector('[role="dialog"]')?.textContent?.replace(/\s+/g, ' ') ?? '';

function rowByName(m: Mounted, name: string): HTMLElement {
  const row = Array.from(m.container.querySelectorAll('.row[data-path]')).find((el) =>
    el.textContent?.includes(name),
  );
  if (!row) throw new Error(`row not found: ${name}`);
  return row as HTMLElement;
}

beforeEach(() => {
  planCopyMock.mockReset();
  enqueueCopyMock.mockReset();
  shellOpenMock.mockReset();
  shellOpenMock.mockResolvedValue(true);
});

afterEach(() => {
  activity.recording = true;
  activity.privateSession = false;
  cleanup();
  dialogs.close();
  endInternalDrag();
});

describe('type-to-select', () => {
  it('cycles matching names in the focused pane and wraps to first match', async () => {
    await boot();

    const body = left.container.querySelector('.body') as HTMLElement;
    fireEvent.keyDown(body, { key: 'ArrowDown' });
    fireEvent.keyDown(body, { key: 'r' });
    await tick();
    expect(rowByName(left, 'renders').classList.contains('cursor')).toBe(true);

    fireEvent.keyDown(body, { key: 'r' });
    await tick();
    expect(rowByName(left, 'README.md').classList.contains('cursor')).toBe(true);

    fireEvent.keyDown(body, { key: 'r' });
    await tick();
    expect(rowByName(left, 'roadmap-2026.pdf').classList.contains('cursor')).toBe(true);

    fireEvent.keyDown(body, { key: 'r' });
    await tick();
    expect(rowByName(left, 'renders').classList.contains('cursor')).toBe(true);
    expect(rowByName(right, 'restore.log').classList.contains('cursor')).toBe(false);
  });

  it('does not move the cursor while a modal is open (FER-577 N4)', async () => {
    await boot();
    const body = left.container.querySelector('.body') as HTMLElement;
    fireEvent.keyDown(body, { key: 'ArrowDown' });
    await tick();
    const before = left.container.querySelector('.row.cursor')?.getAttribute('data-path');
    dialogs.beginMkdir();
    await tick();
    // The key arrives at the window from inside the dialog (focus is on the modal itself).
    fireEvent.keyDown(dialogHost.container.querySelector('.modal') as HTMLElement, { key: 'r' });
    await tick();
    expect(left.container.querySelector('.row.cursor')?.getAttribute('data-path')).toBe(before);
    dialogs.close();
    await tick();
    fireEvent.keyDown(window, { key: 'r' });
    await tick();
    expect(left.container.querySelector('.row.cursor')?.getAttribute('data-path')).not.toBe(before);
  });

  it('ignores modified keys and text input targets', async () => {
    await boot();

    const body = left.container.querySelector('.body') as HTMLElement;
    fireEvent.mouseDown(sectionOf(left));
    fireEvent.keyDown(body, { key: 'r', ctrlKey: true });
    fireEvent.keyDown(window, { key: 'r', altKey: true });
    const input = document.createElement('input');
    document.body.appendChild(input);
    fireEvent.keyDown(input, { key: 'r' });
    await tick();

    expect(rowByName(left, 'README.md').getAttribute('aria-selected')).not.toBe('true');
    expect(rowByName(left, 'roadmap-2026.pdf').getAttribute('aria-selected')).not.toBe('true');
  });
});

describe('cross-pane drag & drop', () => {
  it('drag from pane 0 over pane 1 shows the copy overlay; drop opens the copy dialog and surfaces the enqueue error', async () => {
    planCopyMock.mockResolvedValue({ totalFiles: 1, totalBytes: 2970, conflicts: [] });
    enqueueCopyMock.mockRejectedValue(
      new FsError({
        code: 'unsupported',
        message: 'Filesystem access requires the desktop app (not browser dev)',
      }),
    );
    await boot();

    const row = rowByName(left, 'README.md');
    const dt = fakeDataTransfer();
    fireEvent(row, dragEvent('dragstart', dt));
    fireEvent(sectionOf(right), dragEvent('dragover', dt));
    await tick();
    const overlay = overlayOf(right);
    expect(overlay).toBeTruthy();
    expect(overlay?.textContent).toContain('Copy 1 items');
    expect(overlay?.textContent).toContain('· hold ⇧ to move');

    fireEvent(sectionOf(right), dragEvent('drop', dt));
    await tick();
    expect(overlayOf(right)).toBeNull();

    await vi.waitFor(() => expect(dialogText()).toContain('Copy 1 item'));
    expect(dialogs.current).toMatchObject({
      kind: 'copy',
      target: 'D:\\Backup',
      paths: [row.getAttribute('data-path')],
    });
    expect(planCopyMock).toHaveBeenCalledWith([row.getAttribute('data-path')], 'D:\\Backup');

    // Confirm in browser mode: enqueueCopy rejects 'unsupported' → the dialog
    // stays open with the error, nothing is lost.
    const copyBtn = Array.from(dialogHost.container.querySelectorAll('button')).find(
      (b) => b.textContent === 'Copy',
    );
    expect(copyBtn).toBeTruthy();
    fireEvent.click(copyBtn as HTMLElement);
    await vi.waitFor(() =>
      expect(dialogHost.container.querySelector('[role="dialog"] .error')).toBeTruthy(),
    );
    expect(dialogs.current?.kind).toBe('copy');
  });

  it('shift-drag offers a move (no hint) and drop opens the move dialog', async () => {
    planCopyMock.mockResolvedValue({ totalFiles: 1, totalBytes: 2970, conflicts: [] });
    await boot();

    const dt = fakeDataTransfer();
    fireEvent(rowByName(left, 'README.md'), dragEvent('dragstart', dt));
    fireEvent(sectionOf(right), dragEvent('dragover', dt, true));
    await tick();
    const overlay = overlayOf(right);
    expect(overlay?.textContent).toContain('Move 1 items');
    expect(overlay?.textContent).not.toContain('hold ⇧');

    fireEvent(sectionOf(right), dragEvent('drop', dt, true));
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('move'));
    expect(dialogs.current).toMatchObject({ kind: 'move', target: 'D:\\Backup' });
  });

  it('dragging a selected row carries the whole selection', async () => {
    planCopyMock.mockResolvedValue({ totalFiles: 2, totalBytes: 4096, conflicts: [] });
    await boot();

    const a = rowByName(left, 'README.md');
    const b = rowByName(left, 'meeting-notes.md');
    fireEvent.click(a, { ctrlKey: true });
    fireEvent.click(b, { ctrlKey: true });
    await tick();

    const dt = fakeDataTransfer();
    fireEvent(b, dragEvent('dragstart', dt));
    fireEvent(sectionOf(right), dragEvent('dragover', dt));
    await tick();
    expect(overlayOf(right)?.textContent).toContain('Copy 2 items');

    fireEvent(sectionOf(right), dragEvent('drop', dt));
    await vi.waitFor(() =>
      expect(planCopyMock).toHaveBeenCalledWith(
        [a.getAttribute('data-path'), b.getAttribute('data-path')],
        'D:\\Backup',
      ),
    );
  });

  it('does not accept drops from its own pane (MVP: cross-pane only)', async () => {
    await boot();

    const dt = fakeDataTransfer();
    fireEvent(rowByName(left, 'README.md'), dragEvent('dragstart', dt));
    fireEvent(sectionOf(left), dragEvent('dragover', dt));
    await tick();
    expect(overlayOf(left)).toBeNull();

    fireEvent(sectionOf(left), dragEvent('drop', dt));
    expect(planCopyMock).not.toHaveBeenCalled();
    expect(dialogs.current).toBeNull();
  });

  it('ignores foreign drags that carry no Ferrum payload', async () => {
    await boot();

    const dt = fakeDataTransfer(); // nothing set → no MIME in types
    fireEvent(sectionOf(right), dragEvent('dragover', dt));
    await tick();
    expect(overlayOf(right)).toBeNull();

    fireEvent(sectionOf(right), dragEvent('drop', dt));
    expect(dialogs.current).toBeNull();
    expect(planCopyMock).not.toHaveBeenCalled();
  });

  it('hides the overlay when the drag leaves the pane', async () => {
    await boot();

    const dt = fakeDataTransfer();
    fireEvent(rowByName(left, 'README.md'), dragEvent('dragstart', dt));
    fireEvent(sectionOf(right), dragEvent('dragover', dt));
    await tick();
    expect(overlayOf(right)).toBeTruthy();

    fireEvent(sectionOf(right), dragEvent('dragleave', dt));
    await tick();
    expect(overlayOf(right)).toBeNull();
  });
});

describe('opening files (FER-249)', () => {
  it('double-click on a file opens via the OS once with (path, recordingActive)', async () => {
    await boot();
    const row = rowByName(left, 'README.md');
    await fireEvent.dblClick(row);
    expect(shellOpenMock).toHaveBeenCalledTimes(1);
    expect(shellOpenMock).toHaveBeenCalledWith(row.getAttribute('data-path'), true);
  });

  it('private window or paused recording passes record=false', async () => {
    await boot();
    const row = rowByName(left, 'README.md');
    activity.privateSession = true;
    await fireEvent.dblClick(row);
    expect(shellOpenMock).toHaveBeenLastCalledWith(row.getAttribute('data-path'), false);
    activity.privateSession = false;
    activity.recording = false;
    await fireEvent.dblClick(row);
    expect(shellOpenMock).toHaveBeenLastCalledWith(row.getAttribute('data-path'), false);
    expect(shellOpenMock).toHaveBeenCalledTimes(2);
  });

  it('double-click on a folder navigates and never calls shellOpen', async () => {
    await boot();
    await fireEvent.dblClick(rowByName(left, 'datasets'));
    await tick();
    expect(shellOpenMock).not.toHaveBeenCalled();
  });

  it('Enter path (openCursorEntry) opens the cursor file', async () => {
    await boot();
    // FER-489: the cursor starts on the first row, so Enter acts without a click; clear that.
    shellOpenMock.mockClear();
    const row = rowByName(left, 'meeting-notes.md');
    await fireEvent.click(row);
    await tick();
    getPane(0)?.openCursorEntry();
    expect(shellOpenMock).toHaveBeenCalledTimes(1);
    expect(shellOpenMock).toHaveBeenCalledWith(row.getAttribute('data-path'), true);
  });

  it('shellOpen failure shows an error dialog instead of crashing', async () => {
    shellOpenMock.mockRejectedValue(new FsError({ code: 'unsupported', message: 'no handler' }));
    await boot();
    await fireEvent.dblClick(rowByName(left, 'README.md'));
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('error'));
    expect(dialogs.current).toMatchObject({ title: 'Open', message: 'no handler' });
  });

  it('a declined open (false) is silent', async () => {
    shellOpenMock.mockResolvedValue(false);
    await boot();
    await fireEvent.dblClick(rowByName(left, 'README.md'));
    await tick();
    expect(dialogs.current).toBeNull();
  });

  it('selecting a file never records or opens', async () => {
    await boot();
    await fireEvent.click(rowByName(left, 'README.md'));
    await tick();
    expect(shellOpenMock).not.toHaveBeenCalled();
  });
});

describe('AI context probe (FER-160 wiring)', () => {
  it('registers both panes so a snapshot can be captured from the mounted components', async () => {
    await boot();
    const host = registryHost(() => 'left');
    const snapshot = new ContextService(host).capture('req_probe');
    const [leftPane, rightPane] = snapshot.panes;
    expect([leftPane.paneId, rightPane.paneId]).toEqual(['left', 'right']);
    expect(leftPane.tabId).not.toBe(rightPane.tabId);
    expect(leftPane.location.kind).toBe('disk');
    expect(leftPane.location.path.length).toBeGreaterThan(0);
    // The probe reads the full listing, not only the rendered rows.
    expect(host.probe('left')!.visibleEntries().length).toBeGreaterThan(5);
  });

  it('unregisters the probe when a pane unmounts', async () => {
    await boot();
    const host = registryHost(() => 'left');
    expect(host.probe('left')).toBeDefined();
    cleanup();
    expect(host.probe('left')).toBeUndefined();
    expect(host.probe('right')).toBeUndefined();
  });
});
