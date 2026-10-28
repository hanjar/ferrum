/**
 * FER-590: Shift+F4 "New file" dialog. Submit creates an empty file through `create_entry`,
 * closes the dialog and opens the new file with its default app (`shell_open`). An existing
 * name keeps the dialog open with the error and must not open anything.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';

const mutMocks = vi.hoisted(() => ({
  createEntry: vi.fn(),
}));
vi.mock('../ipc/mutations', () => mutMocks);

const activityMocks = vi.hoisted(() => ({
  shellOpen: vi.fn(async () => true),
}));
vi.mock('../ipc/activity', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../ipc/activity')>()),
  shellOpen: activityMocks.shellOpen,
}));

afterEach(() => {
  dialogs.close();
  mutMocks.createEntry.mockReset();
  activityMocks.shellOpen.mockClear();
});

function typeName(value: string) {
  const input = document.querySelector('[role="dialog"] input') as HTMLInputElement;
  fireEvent.input(input, { target: { value } });
}

function createButton(): HTMLButtonElement {
  return Array.from(document.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')).find(
    (b) => b.textContent?.trim() === 'Create',
  ) as HTMLButtonElement;
}

describe('FER-590: Shift+F4 New file dialog', () => {
  it('renders the field with an empty name', async () => {
    render(Dialogs);
    dialogs.open({ kind: 'newfile', parent: 'C:\t' });
    await tick();
    const modal = document.querySelector('[role="dialog"]');
    expect(modal?.textContent).toContain('New file');
    expect(modal?.textContent).toContain('File name');
    const input = modal?.querySelector('input') as HTMLInputElement;
    expect(input).toBeTruthy();
    expect(input.value).toBe('');
    expect(createButton().disabled).toBe(true);
  });

  it('submit creates the file, then opens the created path', async () => {
    mutMocks.createEntry.mockResolvedValue({ name: 'a.txt', path: 'C:\t\a.txt' });
    render(Dialogs);
    dialogs.open({ kind: 'newfile', parent: 'C:\t' });
    await tick();
    typeName('a.txt');
    await tick();
    fireEvent.click(createButton());
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
    expect(mutMocks.createEntry).toHaveBeenCalledWith('file', 'C:\t', 'a.txt');
    expect(activityMocks.shellOpen).toHaveBeenCalledWith('C:\t\a.txt', expect.any(Boolean));
  });

  it('an AlreadyExists error keeps the dialog open, shows it and opens nothing', async () => {
    mutMocks.createEntry.mockRejectedValue(new Error('AlreadyExists'));
    render(Dialogs);
    dialogs.open({ kind: 'newfile', parent: 'C:\t' });
    await tick();
    typeName('taken.txt');
    await tick();
    fireEvent.click(createButton());
    await vi.waitFor(() => {
      expect(document.querySelector('[role="dialog"] .error')?.textContent).toContain(
        'AlreadyExists',
      );
    });
    expect(dialogs.current).toMatchObject({ kind: 'newfile', parent: 'C:\t' });
    expect(activityMocks.shellOpen).not.toHaveBeenCalled();
  });

  it('a failed open does not reopen the dialog after the file was created', async () => {
    mutMocks.createEntry.mockResolvedValue({ name: 'b.txt', path: 'C:\t\b.txt' });
    activityMocks.shellOpen.mockRejectedValueOnce(new Error('no app'));
    render(Dialogs);
    dialogs.open({ kind: 'newfile', parent: 'C:\t' });
    await tick();
    typeName('b.txt');
    await tick();
    fireEvent.click(createButton());
    await vi.waitFor(() => expect(activityMocks.shellOpen).toHaveBeenCalled());
    await tick();
    expect(dialogs.current).toBeNull();
  });
});
