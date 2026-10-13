import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import CustomColumnsDialog from './CustomColumnsDialog.svelte';
import { columns } from '../stores/columnsStore.svelte';

afterEach(() => {
  cleanup();
  columns.close();
});

describe('CustomColumnsDialog', () => {
  it('takes keyboard focus on open so Escape reaches the dialog (not the row behind it)', async () => {
    const row = document.createElement('button');
    document.body.append(row);
    row.focus();
    columns.openDialog();
    render(CustomColumnsDialog);
    const dialog = await screen.findByRole('dialog', { name: 'Custom Columns' });
    expect(document.activeElement).toBe(dialog);
    await fireEvent.keyDown(document.activeElement as Element, { key: 'Escape' });
    expect(columns.open).toBe(false);
    row.remove();
  });
});
