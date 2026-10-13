/**
 * Global view mode (backlog polish): both panes share one mode for MVP.
 * 'list' intentionally maps to 'details' rendering — prototype parity for
 * the third button lands post-MVP.
 */
export type ViewMode = 'details' | 'grid';

class ViewStore {
  mode = $state<ViewMode>('details');

  set(m: ViewMode) {
    this.mode = m;
  }

  toggle() {
    this.mode = this.mode === 'details' ? 'grid' : 'details';
  }
}

export const view = new ViewStore();
