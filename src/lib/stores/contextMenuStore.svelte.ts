/**
 * Right-click context menu state (one menu app-wide). Pure state + clamping
 * math; `ContextMenu.svelte` renders it and owns the dismiss wiring
 * (Escape / outside mousedown / resize / scroll) via `svelte:window`.
 *
 * `open` takes an optional owner token: FileList exists once per pane, so
 * each instance passes a private token and only the instance that opened the
 * menu renders it (position:fixed duplicates would otherwise stack).
 */

export type ContextMenuItem =
  | { separator: true }
  | {
      label: string;
      /** Mono key hint rendered right-aligned, e.g. 'F5'. */
      kbd?: string;
      danger?: boolean;
      disabled?: boolean;
      run: () => void;
    };

/** Fixed geometry of the rendered menu — mirrors ContextMenu.svelte styles. */
export const MENU_WIDTH = 236;
export const ITEM_HEIGHT = 26;
export const SEPARATOR_HEIGHT = 9; // 1px line + 2×4px margins
export const MENU_PADDING = 8; // 4px top + 4px bottom

/** Estimated rendered height, used to clamp the position before first paint. */
export function estimateMenuHeight(items: ContextMenuItem[]): number {
  let h = MENU_PADDING;
  for (const it of items) h += 'separator' in it ? SEPARATOR_HEIGHT : ITEM_HEIGHT;
  return h;
}

/** Keep the menu rect inside the viewport; never negative on tiny windows. */
export function clampMenuPosition(
  x: number,
  y: number,
  menuHeight: number,
  viewportW: number,
  viewportH: number,
): { x: number; y: number } {
  return {
    x: Math.max(0, Math.min(x, viewportW - MENU_WIDTH)),
    y: Math.max(0, Math.min(y, viewportH - menuHeight)),
  };
}

class ContextMenuStore {
  isOpen = $state(false);
  x = $state(0);
  y = $state(0);
  // Raw: items/owner are replaced wholesale (never mutated) and consumers
  // rely on their identity — $state's deep proxy would break `===` on tokens.
  items = $state.raw<ContextMenuItem[]>([]);
  owner = $state.raw<unknown>(undefined);

  /** Show the menu at viewport coords, clamped so it never leaves the screen. */
  open(x: number, y: number, items: ContextMenuItem[], owner?: unknown): void {
    this.items = items;
    this.owner = owner;
    const pos = clampMenuPosition(
      x,
      y,
      estimateMenuHeight(items),
      window.innerWidth,
      window.innerHeight,
    );
    this.x = pos.x;
    this.y = pos.y;
    this.isOpen = true;
  }

  close(): void {
    this.isOpen = false;
  }
}

export const contextMenu = new ContextMenuStore();

export interface MenuFocusReturn<T = string> {
  activeId: T | null;
  setOpener(el: HTMLElement | undefined, id?: T | null): void;
  clear(): void;
}

/**
 * Shared helper for components opening contextMenu from trigger buttons (e.g. action dropdowns).
 * Retains the trigger element and restores focus to it when the menu closes,
 * tracking the currently active item id for aria-expanded states.
 */
export function createMenuFocusReturn<T = string>(): MenuFocusReturn<T> {
  let opener: HTMLElement | undefined;
  let activeId = $state<T | null>(null);

  $effect(() => {
    if (contextMenu.isOpen) return;
    activeId = null;
    const el = opener;
    opener = undefined;
    if (el?.isConnected) el.focus();
  });

  return {
    get activeId(): T | null {
      return activeId;
    },
    set activeId(v: T | null) {
      activeId = v;
    },
    setOpener(el: HTMLElement | undefined, id?: T | null): void {
      opener = el;
      if (id !== undefined) activeId = id;
    },
    clear(): void {
      opener = undefined;
      activeId = null;
    },
  };
}
