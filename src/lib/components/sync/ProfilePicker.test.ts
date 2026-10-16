import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import ProfilePicker from './ProfilePicker.svelte';
import type { ProfileSummary } from '../../ipc/syncProfiles';

const p = (id: string, readiness: ProfileSummary['readiness']): ProfileSummary => ({
  id,
  name: `Profile ${id}`,
  revision: 1,
  left: 'D:\\A',
  right: 'E:\\B',
  modeLabel: 'Mirror left → right',
  deleteLabel: 'Recycle Bin',
  readiness,
  usedBy: 0,
});

describe('ProfilePicker', () => {
  const profiles = [
    p('a', { status: 'ready' }),
    p('b', { status: 'notReady', reason: 'identityMismatch' }),
    p('c', { status: 'checking' }),
  ];

  it('renders Ready cards enabled and Not-ready cards disabled with the reason', () => {
    render(ProfilePicker, { profiles });
    const ready = screen.getByRole('radio', { name: /Profile a/ }) as HTMLButtonElement;
    const notReady = screen.getByRole('radio', { name: /Profile b/ }) as HTMLButtonElement;
    const checking = screen.getByRole('radio', { name: /Profile c/ }) as HTMLButtonElement;
    expect(ready.disabled).toBe(false);
    expect(notReady.disabled).toBe(true);
    expect(notReady.getAttribute('aria-disabled')).toBe('true');
    expect(notReady.textContent).toContain('Not ready');
    expect(notReady.textContent).toMatch(/different drive/);
    expect(checking.disabled).toBe(true);
    expect(checking.textContent).toContain('Checking');
  });

  it('selects only a Ready profile', async () => {
    const onselect = vi.fn();
    render(ProfilePicker, { profiles, onselect, selectedId: 'a' });
    await fireEvent.click(screen.getByRole('radio', { name: /Profile b/ }));
    expect(onselect).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('radio', { name: /Profile a/ }));
    expect(onselect).toHaveBeenCalledWith('a');
    expect(screen.getByRole('radio', { name: /Profile a/ }).getAttribute('aria-checked')).toBe(
      'true',
    );
  });
});
