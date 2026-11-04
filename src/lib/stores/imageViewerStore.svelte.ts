/**
 * FER-593: state of the F3 image viewer overlay. The list is a snapshot of the pane's images in
 * its current listing order (sorted, filtered); navigation never wraps. Closing puts the pane
 * cursor on the image shown last.
 */
import { getPane, type PaneEntryInfo } from './panesRegistry.svelte';
import type { PaneId } from '../uiState.svelte';

const VIEWABLE = /\.(png|jpe?g|gif|webp|bmp|ico|tiff?|svg)$/i;

export function isViewableImage(name: string): boolean {
  return VIEWABLE.test(name);
}

class ImageViewerStore {
  isOpen = $state(false);
  pane = $state<PaneId>(0);
  /** Image paths in listing order. */
  paths = $state<string[]>([]);
  index = $state(0);

  get path(): string | undefined {
    return this.paths[this.index];
  }

  /** Open on `path`; `entries` is the pane's visible listing (non-images are skipped). */
  show(
    pane: PaneId,
    entries: readonly Pick<PaneEntryInfo, 'name' | 'path' | 'isDir'>[],
    path: string,
  ) {
    const paths = entries.filter((e) => !e.isDir && isViewableImage(e.name)).map((e) => e.path);
    const at = paths.indexOf(path);
    if (at < 0) return;
    this.pane = pane;
    this.paths = paths;
    this.index = at;
    this.isOpen = true;
  }

  /** Step by `delta` images, stopping at the ends. */
  move(delta: number) {
    this.index = Math.min(this.paths.length - 1, Math.max(0, this.index + delta));
  }

  first() {
    this.index = 0;
  }

  last() {
    this.index = this.paths.length - 1;
  }

  /** Close and put the pane cursor on the image shown last. */
  close() {
    if (!this.isOpen) return;
    const last = this.path;
    this.isOpen = false;
    this.paths = [];
    this.index = 0;
    const pane = getPane(this.pane);
    if (last !== undefined) pane?.setCursor?.(last);
    pane?.focusList?.();
  }

  /** Test hook. */
  reset() {
    this.isOpen = false;
    this.paths = [];
    this.index = 0;
  }
}

export const imageViewer = new ImageViewerStore();
