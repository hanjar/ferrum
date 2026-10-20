// FER-576: the real navigation port: the disk answer decides, the pane must really show the result.
import { describe, expect, it } from 'vitest';
import { createNavigatePort, type NavigatePortDeps } from './navigatePort';
import { FakeHost } from './contextTestKit';

function setup(over: Partial<NavigatePortDeps> = {}) {
  const host = new FakeHost();
  const opened: string[] = [];
  const deps: NavigatePortDeps = {
    available: () => true,
    resolvePath: async (path) => ({ path, kind: 'dir' }),
    pane: (side) => ({
      openPath: (path) => {
        opened.push(`open:${side}:${path}`);
        const pane = side === 'left' ? host.left : host.right;
        pane.path = path;
      },
      revealPath: (path) => {
        opened.push(`reveal:${side}:${path}`);
        const pane = side === 'left' ? host.left : host.right;
        pane.path = path.slice(0, path.lastIndexOf('\\'));
        pane.selected = [path];
      },
    }),
    probe: (side) => host.probe(side),
    timeoutMs: 200,
    pollMs: 5,
    ...over,
  };
  return { host, opened, port: createNavigatePort(deps) };
}

const FILE = { kind: 'file' as const, path: 'D:\\Downloads\\a.csv', folder: 'D:\\Downloads' };

describe('createNavigatePort', () => {
  it('classifies a folder, a file and a missing path from the disk answer', async () => {
    const a = setup({ resolvePath: async (path) => ({ path, kind: 'dir' }) });
    expect(await a.port.resolve('D:\\Downloads')).toEqual({ kind: 'dir', path: 'D:\\Downloads' });
    const b = setup({ resolvePath: async (path) => ({ path, kind: 'file' }) });
    expect(await b.port.resolve('D:\\Downloads\\a.csv')).toEqual(FILE);
    const c = setup({ resolvePath: async (path) => ({ path, kind: 'missing' }) });
    expect(await c.port.resolve('D:\\nope')).toEqual({ kind: 'missing' });
  });

  it('never enters an archive: an archive file is revealed in its folder', async () => {
    const a = setup({ resolvePath: async (path) => ({ path, kind: 'archive' }) });
    expect(await a.port.resolve('D:\\x\\a.zip')).toMatchObject({ kind: 'file', folder: 'D:\\x' });
  });

  it('treats a path the resolver changed (for example %VAR% expansion) as missing', async () => {
    const a = setup({
      resolvePath: async () => ({ path: 'C:\\Users\\me\\AppData', kind: 'dir' }),
    });
    expect(await a.port.resolve('C:\\%APPDATA%')).toEqual({ kind: 'missing' });
  });

  it('opens a folder and reports success only when the pane shows it', async () => {
    const a = setup();
    expect(await a.port.open('left', { kind: 'dir', path: 'D:\\Downloads' })).toBe(true);
    expect(a.opened).toEqual(['open:left:D:\\Downloads']);
    const never = setup({ pane: () => ({ openPath: () => undefined }) });
    expect(await never.port.open('left', { kind: 'dir', path: 'D:\\Downloads' })).toBe(false);
  });

  it('reveals a file only when the folder shows AND the file is selected', async () => {
    const a = setup();
    expect(await a.port.open('right', FILE)).toBe(true);
    expect(a.host.right.selected).toEqual(['D:\\Downloads\\a.csv']);
    const holder: { host?: FakeHost } = {};
    const noSelect = setup({
      pane: (side) => ({
        revealPath: (path) => {
          const pane = side === 'left' ? holder.host!.left : holder.host!.right;
          pane.path = path.slice(0, path.lastIndexOf('\\'));
        },
      }),
    });
    holder.host = noSelect.host;
    expect(await noSelect.port.open('left', FILE)).toBe(false);
  });

  it('refuses when the pane is missing or cannot open', async () => {
    const dir = { kind: 'dir' as const, path: 'D:\\x' };
    expect(await setup({ pane: () => undefined }).port.open('left', dir)).toBe(false);
    expect(await setup({ pane: () => ({}) }).port.open('left', dir)).toBe(false);
  });

  it('settles once the listings stop changing', async () => {
    const a = setup();
    let ticks = 0;
    const timer = setInterval(() => {
      a.host.left.listing = {};
      if (++ticks >= 3) clearInterval(timer);
    }, 4);
    await a.port.settled!();
    expect(ticks).toBe(3);
  });

  it('is available only while the host says so', () => {
    expect(setup({ available: () => false }).port.available()).toBe(false);
    expect(setup().port.available()).toBe(true);
  });
});
