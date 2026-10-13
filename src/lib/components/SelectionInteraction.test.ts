import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import FilePane from './FilePane.svelte';
import { ui } from '../uiState.svelte';

afterEach(() => {
  ui.activePane = 0;
});

function pane() {
  return render(FilePane, { paneId: 0 });
}

function rows(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll('.row[data-path]')) as HTMLElement[];
}

function statusOf(container: HTMLElement): string | undefined {
  return container.querySelector('.status .info')?.textContent ?? undefined;
}

/** Status text without the marked-size suffix ("2 of 13 selected · 1.2 KB"). */
const count = (container: HTMLElement) => statusOf(container)?.replace(/ · .*$/, '');

async function boot() {
  const r = pane();
  await vi.waitFor(() => expect(rows(r.container).length).toBeGreaterThan(5));
  return r;
}

// FER-492 (Total Commander model): the cursor and the marks are separate.
describe('selection interaction (task 08, FER-492 cursor/mark model)', () => {
  it('plain click moves the cursor and marks nothing', async () => {
    const { container } = await boot();
    const first = rows(container).find((el) => el.textContent?.includes('archive-2025'))!;
    fireEvent.click(first);
    await tick();
    expect(container.querySelector('.row.cursor')?.textContent).toContain('archive-2025');
    expect(container.querySelectorAll('.row.selected')).toHaveLength(0);
    expect(statusOf(container)).toBe('13 items');
  });

  it('ctrl+click toggles marks; a plain click clears them', async () => {
    const { container } = await boot();
    const r = rows(container);
    const a = r.find((el) => el.textContent?.includes('archive-2025'))!;
    const b = r.find((el) => el.textContent?.includes('datasets'))!;
    fireEvent.click(a, { ctrlKey: true });
    fireEvent.click(b, { ctrlKey: true });
    await tick();
    expect(count(container)).toBe('2 of 13 selected');
    fireEvent.click(b, { ctrlKey: true }); // toggle off
    await tick();
    expect(count(container)).toBe('1 of 13 selected');
    fireEvent.click(b); // plain click clears the marks
    await tick();
    expect(statusOf(container)).toBe('13 items');
  });

  it('shift+click marks a range from the anchor', async () => {
    const { container } = await boot();
    const r = rows(container);
    const name = (el: HTMLElement) => el.getAttribute('data-path')!.split('\\').pop()!;
    const a = r.find((el) => name(el) === 'archive-2025')!;
    const b = r.find((el) => name(el) === 'datasets')!;
    fireEvent.click(a);
    fireEvent.click(b, { shiftKey: true });
    await tick();
    // visible order is sorted: archive-2025, atlas-design-system, datasets, …
    expect(count(container)).toBe('3 of 13 selected');
  });

  it('arrow keys only move the cursor and keep existing marks', async () => {
    const { container } = await boot();
    const body = container.querySelector('.body') as HTMLElement;
    const first = rows(container).find((el) => el.textContent?.includes('archive-2025'))!;
    fireEvent.click(first, { ctrlKey: true });
    await tick();
    expect(count(container)).toBe('1 of 13 selected');

    fireEvent.keyDown(body, { key: 'ArrowDown' });
    fireEvent.keyDown(body, { key: 'ArrowDown', ctrlKey: true });
    await tick();
    expect(count(container)).toBe('1 of 13 selected'); // marks unchanged
    expect(container.querySelector('.row.cursor')?.getAttribute('data-path')).not.toBe(
      first.getAttribute('data-path'),
    );
  });

  it('Space marks the cursor row and steps down; Space again on a marked row unmarks', async () => {
    const { container } = await boot();
    const body = container.querySelector('.body') as HTMLElement;
    fireEvent.click(rows(container).find((el) => el.textContent?.includes('archive-2025'))!);
    fireEvent.keyDown(body, { key: ' ' });
    fireEvent.keyDown(body, { key: 'ArrowDown' });
    fireEvent.keyDown(body, { key: ' ' });
    await tick();
    expect(count(container)).toBe('2 of 13 selected');
    fireEvent.keyDown(body, { key: 'ArrowUp' });
    fireEvent.keyDown(body, { key: ' ' });
    await tick();
    expect(count(container)).toBe('1 of 13 selected');
  });

  it('ctrl+a marks everything visible; Escape clears the marks', async () => {
    const { container } = await boot();
    const body = container.querySelector('.body') as HTMLElement;
    body.focus();
    fireEvent.keyDown(body, { key: 'a', ctrlKey: true });
    await tick();
    expect(count(container)).toBe('13 of 13 selected');
    fireEvent.keyDown(body, { key: 'Escape' });
    await tick();
    expect(statusOf(container)).toBe('13 items');
  });

  it('shift+arrows mark a range', async () => {
    const { container } = await boot();
    const body = container.querySelector('.body') as HTMLElement;
    fireEvent.click(rows(container).find((el) => el.textContent?.includes('archive-2025'))!);
    await tick();
    fireEvent.keyDown(body, { key: 'ArrowDown', shiftKey: true });
    await tick();
    expect(count(container)).toBe('2 of 13 selected');
  });
});

describe('sorting interaction (task 08)', () => {
  it('clicking the Size header sorts by size descending-then-ascending', async () => {
    const { container } = await boot();
    const sizeHead = Array.from(container.querySelectorAll('.head button')).find((b) =>
      b.textContent?.includes('Size'),
    ) as HTMLElement;
    // 6 folders always lead the list; judge sorting by the first FILE row.
    const firstFileRow = () => rows(container)[6].getAttribute('data-path')!.split('\\').pop();

    fireEvent.click(sizeHead); // size asc
    await tick();
    expect(firstFileRow()).toBe('workspace.code-workspace'); // 612 B
    fireEvent.click(sizeHead); // size desc
    await tick();
    expect(firstFileRow()).toBe('kickoff-recording.mp4'); // 1.3 GB
  });

  it('marks survive a re-sort (path identity)', async () => {
    const { container } = await boot();
    const r = rows(container);
    const target = r.find((el) => el.textContent?.includes('README.md'))!;
    fireEvent.click(target, { ctrlKey: true });
    await tick();
    expect(count(container)).toBe('1 of 13 selected');

    const nameHead = Array.from(container.querySelectorAll('.head button'))[0];
    fireEvent.click(nameHead); // name desc
    await tick();
    expect(count(container)).toBe('1 of 13 selected');
    expect(container.querySelectorAll('.row.selected')).toHaveLength(1);
    expect(container.querySelector('.row.selected')?.textContent).toContain('README.md');
  });
});

describe('filter interaction (task 08)', () => {
  it('filtering narrows the list to matches; marks are preserved for kept rows', async () => {
    const { container } = await boot();
    (container.querySelector('.filterBtn') as HTMLElement).click();
    await tick();
    const input = container.querySelector('.filter input') as HTMLInputElement;
    fireEvent.input(input, { target: { value: 'md' } });
    await tick();
    const r = rows(container);
    expect(r.length).toBe(2); // README.md, meeting-notes.md
    const readme = r.find((el) => el.textContent?.includes('README.md'))!;
    fireEvent.click(readme, { ctrlKey: true });
    await tick();
    expect(count(container)).toBe('1 of 13 selected');
  });
});
