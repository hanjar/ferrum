import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import ColumnResizeHandle from './ColumnResizeHandle.svelte';

beforeEach(() => {
  document.body.style.cursor = '';
  document.body.style.userSelect = '';
});

afterEach(() => {
  cleanup();
});

describe('ColumnResizeHandle', () => {
  it('renders with ARIA separator semantics and initial value', () => {
    const { getByRole } = render(ColumnResizeHandle, {
      label: 'Ubah lebar kolom Nama',
      value: 150,
      min: 120,
      max: 2000,
      step: 16,
      onchange: vi.fn(),
    });

    const handle = getByRole('separator');
    expect(handle.getAttribute('aria-orientation')).toBe('vertical');
    expect(handle.getAttribute('aria-label')).toBe('Ubah lebar kolom Nama');
    expect(handle.getAttribute('aria-valuemin')).toBe('120');
    expect(handle.getAttribute('aria-valuemax')).toBe('2000');
    expect(handle.getAttribute('aria-valuenow')).toBe('150');
    expect(handle.getAttribute('tabindex')).toBe('0');
  });

  it('drags with pointer capture: invokes onchange on move and oncommit on up', () => {
    const onchange = vi.fn();
    const oncommit = vi.fn();
    const { getByRole } = render(ColumnResizeHandle, {
      label: 'Jenis',
      value: 90,
      min: 56,
      max: 2000,
      step: 16,
      onchange,
      oncommit,
    });

    const handle = getByRole('separator');
    handle.setPointerCapture = vi.fn();
    handle.hasPointerCapture = vi.fn(() => true);
    handle.releasePointerCapture = vi.fn();

    // Start drag at x = 100
    fireEvent.pointerDown(handle, { button: 0, clientX: 100, pointerId: 1 });
    expect(handle.setPointerCapture).toHaveBeenCalledWith(1);

    // Move to x = 140 (delta +40) -> 90 + 40 = 130
    fireEvent.pointerMove(handle, { clientX: 140, pointerId: 1 });
    expect(onchange).toHaveBeenCalledWith(130);
    // Oncommit must NOT be called on move
    expect(oncommit).not.toHaveBeenCalled();

    // Release at x = 140
    fireEvent.pointerUp(handle, { clientX: 140, pointerId: 1 });
    expect(oncommit).toHaveBeenCalledWith(130);
    expect(handle.releasePointerCapture).toHaveBeenCalledWith(1);
  });

  it('clamps drag width to min and max', () => {
    const onchange = vi.fn();
    const oncommit = vi.fn();
    const { getByRole } = render(ColumnResizeHandle, {
      label: 'Jenis',
      value: 90,
      min: 56,
      max: 200,
      step: 16,
      onchange,
      oncommit,
    });

    const handle = getByRole('separator');

    // Drag far left
    fireEvent.pointerDown(handle, { button: 0, clientX: 100, pointerId: 1 });
    fireEvent.pointerMove(handle, { clientX: 0, pointerId: 1 }); // delta -100 -> 90 - 100 = -10 -> clamp to 56
    expect(onchange).toHaveBeenLastCalledWith(56);
    fireEvent.pointerUp(handle, { clientX: 0, pointerId: 1 });
    expect(oncommit).toHaveBeenLastCalledWith(56);

    // Drag far right past max 200
    fireEvent.pointerDown(handle, { button: 0, clientX: 100, pointerId: 1 });
    fireEvent.pointerMove(handle, { clientX: 500, pointerId: 1 }); // delta +400 -> 90 + 400 = 490 -> clamp to 200
    expect(onchange).toHaveBeenLastCalledWith(200);
    fireEvent.pointerUp(handle, { clientX: 500, pointerId: 1 });
    expect(oncommit).toHaveBeenLastCalledWith(200);
  });

  it('handles keyboard navigation (ArrowLeft, ArrowRight, Home, End)', () => {
    const onchange = vi.fn();
    const oncommit = vi.fn();
    const { getByRole } = render(ColumnResizeHandle, {
      label: 'Ukuran',
      value: 100,
      min: 56,
      max: 500,
      step: 16,
      onchange,
      oncommit,
    });

    const handle = getByRole('separator');

    // ArrowRight: 100 + 16 = 116
    fireEvent.keyDown(handle, { key: 'ArrowRight' });
    expect(onchange).toHaveBeenLastCalledWith(116);
    expect(oncommit).toHaveBeenLastCalledWith(116);

    // ArrowLeft: 100 - 16 = 84
    fireEvent.keyDown(handle, { key: 'ArrowLeft' });
    expect(onchange).toHaveBeenLastCalledWith(84);
    expect(oncommit).toHaveBeenLastCalledWith(84);

    // Home: jumps to min 56
    fireEvent.keyDown(handle, { key: 'Home' });
    expect(onchange).toHaveBeenLastCalledWith(56);
    expect(oncommit).toHaveBeenLastCalledWith(56);

    // End: jumps to max 500
    fireEvent.keyDown(handle, { key: 'End' });
    expect(onchange).toHaveBeenLastCalledWith(500);
    expect(oncommit).toHaveBeenLastCalledWith(500);
  });

  it('triggers onreset on double-click', () => {
    const onreset = vi.fn();
    const { getByRole } = render(ColumnResizeHandle, {
      label: 'Ukuran',
      value: 120,
      min: 56,
      onchange: vi.fn(),
      onreset,
    });

    const handle = getByRole('separator');
    fireEvent.dblClick(handle);
    expect(onreset).toHaveBeenCalledTimes(1);
  });
});
