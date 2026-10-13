import { afterEach, describe, expect, it } from 'vitest';
import { focusModal } from './focusModal';

afterEach(() => {
  document.body.innerHTML = '';
});

describe('focusModal', () => {
  it('moves focus into the modal on open and hands it back to the opener on destroy', () => {
    const opener = document.createElement('button');
    document.body.append(opener);
    opener.focus();
    const modal = document.createElement('div');
    modal.tabIndex = -1;
    document.body.append(modal);

    const action = focusModal(modal);
    // The regression: focus stayed on the row behind the scrim, so Escape on
    // the modal's own keydown handler never fired.
    expect(document.activeElement).toBe(modal);

    action.destroy();
    expect(document.activeElement).toBe(opener);
  });
});
