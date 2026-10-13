import type { ContextEntry, ContextHost, PaneProbe } from './context';

// Test kit: in-memory fake host untuk tes modul context*.
export function entry(name: string, over: Partial<ContextEntry> = {}): ContextEntry {
  return {
    name,
    path: `D:\\Work\\${name}`,
    isDir: false,
    isSymlink: false,
    hidden: false,
    ...over,
  };
}

export class FakePane {
  tab: object = {};
  listing: object = {};
  viewKind: PaneProbe['viewKind'] = 'directory';
  path = 'D:\\Work';
  filter = { mode: 'all', pattern: '', query: '' };
  showHidden = false;
  sort: PaneProbe['sort'] = { key: 'name', dir: 1 };
  all: ContextEntry[] = [];
  selected: string[] = [];
  constructor(public paneId: 'left' | 'right') {}
  /** Visible = all minus hidden (unless showHidden) and name query. */
  visible(): ContextEntry[] {
    const q = this.filter.query.toLowerCase();
    return this.all.filter(
      (e) => (this.showHidden || !e.hidden) && (!q || e.name.toLowerCase().includes(q)),
    );
  }
  probe(): PaneProbe {
    return {
      paneId: this.paneId,
      tab: this.tab,
      listing: this.listing,
      viewKind: this.viewKind,
      path: this.path,
      filter: this.filter,
      showHidden: this.showHidden,
      sort: this.sort,
      selectedPaths: () => this.selected,
      visibleEntries: () => this.visible(),
      allEntries: () => this.all,
    };
  }
}

export class FakeHost implements ContextHost {
  left = new FakePane('left');
  right = new FakePane('right');
  active: 'left' | 'right' = 'left';
  constructor() {
    this.right.path = 'E:\\Backup';
  }
  activePane() {
    return this.active;
  }
  probe(p: 'left' | 'right'): PaneProbe | undefined {
    return (p === 'left' ? this.left : this.right).probe();
  }
  swap() {
    [this.left.tab, this.right.tab] = [this.right.tab, this.left.tab];
    [this.left.listing, this.right.listing] = [this.right.listing, this.left.listing];
    [this.left.path, this.right.path] = [this.right.path, this.left.path];
    [this.left.all, this.right.all] = [this.right.all, this.left.all];
    [this.left.selected, this.right.selected] = [this.right.selected, this.left.selected];
  }
}
