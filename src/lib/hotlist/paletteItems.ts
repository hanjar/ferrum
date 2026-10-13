/**
 * Hotlist entries for the Command Palette (FER-481): the nested hotlist tree flattened into plain
 * rows with a breadcrumb label, plus the matcher. Pure, so it is unit tested without the UI.
 */
import { rankMatches, type MatchField } from '../paletteMatch';
import { isNetworkPath } from '../networkPath';
import { commandLine, titleParts, type CommandItem, type DirItem, type HotItem } from './model';

export interface PaletteHotItem {
  /** Stable id: the index path inside the tree (`0.1.2`). */
  id: string;
  /** The same index path as numbers (what the backend needs to run a command item). */
  position: number[];
  /** FER-467 R8: a folder (or its target) on a network share: opening it contacts that host. */
  network: boolean;
  kind: 'dir' | 'command';
  /** `Drive › personal › Alice`; `&` accelerators and leading numbers are removed. */
  breadcrumb: string;
  /** Folder path or command line, shown dimmed next to the label. */
  detail: string;
  item: DirItem | CommandItem;
}

/**
 * Flatten the tree in hotlist order. Separators and empty submenus produce nothing. A folder whose
 * label repeats its parent submenu's (TC's habit of a "Drive" folder inside "Drive") is not
 * shown twice in the breadcrumb.
 */
export function flattenHotlist(items: HotItem[]): PaletteHotItem[] {
  const out: PaletteHotItem[] = [];
  const walk = (list: HotItem[], trail: string[], idPath: number[]): void => {
    list.forEach((it, i) => {
      const id = [...idPath, i];
      if (it.type === 'separator') return;
      const label = titleParts(it.title).label.trim();
      if (it.type === 'submenu') {
        walk(it.items, [...trail, label], id);
        return;
      }
      const segs = trail[trail.length - 1] === label ? trail : [...trail, label];
      out.push({
        id: id.join('.'),
        position: id,
        network: it.type === 'dir' && (isNetworkPath(it.path) || isNetworkPath(it.targetPath)),
        kind: it.type,
        breadcrumb: segs.join(' › '),
        detail: it.type === 'dir' ? it.path : commandLine(it.command, it.args),
        item: it,
      });
    });
  };
  walk(items, [], []);
  return out;
}

/** Last non-empty segment of a path (either separator style). */
function basename(path: string): string {
  const parts = path.split(/[\\/]+/).filter(Boolean);
  return parts.length > 0 ? parts[parts.length - 1] : path;
}

function hotFields(e: PaletteHotItem): MatchField[] {
  const lastCrumb = e.breadcrumb.slice(e.breadcrumb.lastIndexOf('›') + 1).trim();
  const target =
    e.item.type === 'dir' ? e.item.path : e.item.type === 'command' ? e.item.command : '';
  return [
    { text: e.breadcrumb },
    { text: lastCrumb, weight: 1.1 },
    // The folder's own name outweighs the rest of the path (FER-485).
    { text: basename(target), weight: 1.1 },
    { text: e.detail, weight: 0.7, fuzzy: false },
  ];
}

/**
 * Typo-tolerant (see `paletteMatch`): every whitespace-separated word must match the breadcrumb,
 * the title, the path's basename or the full path. Best match first; ties keep hotlist order.
 */
export function matchHotlist(entries: PaletteHotItem[], query: string): PaletteHotItem[] {
  return rankMatches(entries, query, hotFields);
}
