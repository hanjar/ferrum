import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke, isTauri: () => false }));

import AutomationPage from './AutomationPage.svelte';
import { AutomationStore } from '../../stores/automationStore.svelte';
import {
  RUNTIME_APP_DESC_WATCH,
  UNSUPPORTED_ROOT_NOTE,
  WATCH_NOTIFICATION_NOTE,
} from './automationModel';
import { fakeBackend, fakeProfiles, profileSummary } from './automationTestKit';

function setup(profiles = [profileSummary()]) {
  const backend = fakeBackend();
  const profilesBackend = fakeProfiles(profiles);
  const store = new AutomationStore({ backend, profiles: profilesBackend, validateDelayMs: 0 });
  render(AutomationPage, { store });
  return { backend, profilesBackend, store };
}

async function openWatchEditor() {
  await fireEvent.click(await screen.findByRole('button', { name: 'New automation' }));
  await fireEvent.click(await screen.findByRole('radio', { name: 'When files change' }));
  await screen.findByTestId('watch-fields');
}

const idle = () => screen.getByRole('textbox', { name: 'Idle delay' }) as HTMLInputElement;
const wait = () => screen.getByRole('textbox', { name: 'Longest wait' }) as HTMLInputElement;
const checkbox = (name: string) => screen.getByRole('checkbox', { name }) as HTMLInputElement;

async function typeInto(el: HTMLInputElement, value: string) {
  await fireEvent.input(el, { target: { value } });
  await fireEvent.change(el);
  await tick();
}

beforeEach(() => invoke.mockReset());
afterEach(() => vi.useRealTimers());

describe('Watch fields in the editor (WA-1.3, WA-2)', () => {
  it('shows the watch fields instead of the schedule, with the runtime text and the WA-2.4 note', async () => {
    setup();
    await openWatchEditor();
    expect(screen.queryByRole('radiogroup', { name: 'Frequency' })).toBeNull();
    expect(screen.queryByText('If a run is missed')).toBeNull();
    expect(screen.getByTestId('watch-note').textContent).toBe(WATCH_NOTIFICATION_NOTE);
    expect(screen.getByText(RUNTIME_APP_DESC_WATCH)).toBeTruthy();
    expect(checkbox('Watch the left folder').checked).toBe(true);
    expect(checkbox('Watch the right folder').checked).toBe(false);
    expect(checkbox('File & folder changes').checked).toBe(true);
    expect(checkbox('Location becomes available').checked).toBe(true);
    expect(idle().value).toBe('10');
    expect(wait().value).toBe('15');
  });

  it('the idle delay stays an integer of 2 to 3600, typed or stepped (WA-1.3)', async () => {
    const { store } = setup();
    await openWatchEditor();
    const form = () => store.editor!.form;

    await typeInto(idle(), '1');
    expect(form().idleSecs).toBe(2);
    expect(idle().value).toBe('2');

    await typeInto(idle(), '99999');
    expect(form().idleSecs).toBe(3600);
    expect(idle().value).toBe('3600');

    await typeInto(idle(), '0');
    expect(form().idleSecs).toBe(2);
    await typeInto(idle(), '-5');
    expect(form().idleSecs).toBe(2);
    await typeInto(idle(), '12.6');
    expect(form().idleSecs).toBe(13);
    expect(idle().value).toBe('13');
    // Text that is not a number falls back to the default instead of storing garbage.
    await typeInto(idle(), 'abc');
    expect(form().idleSecs).toBe(10);
    expect(idle().value).toBe('10');
    await typeInto(idle(), '');
    expect(form().idleSecs).toBe(10);

    // The stepper moves by one and stops at the ends.
    await typeInto(idle(), '2');
    const shorter = screen.getByRole('button', { name: 'Shorter idle delay' }) as HTMLButtonElement;
    const longer = screen.getByRole('button', { name: 'Longer idle delay' }) as HTMLButtonElement;
    expect(shorter.disabled).toBe(true);
    await fireEvent.click(longer);
    expect(form().idleSecs).toBe(3);
    expect(idle().value).toBe('3');
    await fireEvent.click(shorter);
    expect(form().idleSecs).toBe(2);
    await typeInto(idle(), '3600');
    expect(longer.disabled).toBe(true);
    await fireEvent.click(shorter);
    expect(form().idleSecs).toBe(3599);
  });

  it('the longest wait is edited in minutes and clamped to 1 to 1440', async () => {
    const { store } = setup();
    await openWatchEditor();
    await typeInto(wait(), '0');
    expect(store.editor!.form.maxWaitSecs).toBe(60);
    await typeInto(wait(), '5000');
    expect(store.editor!.form.maxWaitSecs).toBe(86400);
    await typeInto(wait(), '30');
    expect(store.editor!.form.maxWaitSecs).toBe(1800);
  });

  it('a root without change notifications is disabled, unticked and carries its note (WA-2.3)', async () => {
    setup([profileSummary({ left: 'C:\\Photos', right: '\\\\nas\\photos' })]);
    await openWatchEditor();
    const right = checkbox('Watch the right folder');
    expect(right.disabled).toBe(true);
    expect(right.checked).toBe(false);
    expect(screen.getByTestId('unsupported-right').textContent).toBe(UNSUPPORTED_ROOT_NOTE);
    // The other root is selectable.
    const left = checkbox('Watch the left folder');
    expect(left.disabled).toBe(false);
    expect(screen.queryByTestId('unsupported-left')).toBeNull();
  });

  it('with no watchable root the editor lists the error and Continue stays disabled', async () => {
    setup([profileSummary({ left: '\\\\nas\\a', right: '\\\\nas\\b' })]);
    await openWatchEditor();
    expect(checkbox('Watch the left folder').disabled).toBe(true);
    expect(checkbox('Watch the right folder').disabled).toBe(true);
    expect((await screen.findByTestId('editor-errors')).textContent).toContain(
      'Pick at least one folder to watch',
    );
    expect(
      (screen.getByRole('button', { name: 'Continue to review' }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it('Run when needs at least one trigger', async () => {
    setup();
    await openWatchEditor();
    await fireEvent.click(checkbox('File & folder changes'));
    await fireEvent.click(checkbox('Location becomes available'));
    expect((await screen.findByTestId('editor-errors')).textContent).toContain(
      'Pick when the automation should run',
    );
  });
});

describe('Simulate a change (WA-8)', () => {
  const callsOf = (b: object) =>
    Object.fromEntries(
      Object.entries(b).map(([k, v]) => [
        k,
        (v as { mock: { calls: unknown[] } }).mock.calls.length,
      ]),
    );

  it('counts down from the idle delay, resets on every press, ends with the preview text and never calls IPC', async () => {
    const { backend, profilesBackend, store } = setup();
    await openWatchEditor();
    await typeInto(idle(), '5');
    expect(store.editor!.form.idleSecs).toBe(5);
    // Let the validation of the edit finish before the counters are taken.
    await new Promise((r) => setTimeout(r, 0));

    vi.useFakeTimers();
    const before = callsOf(backend);
    const beforeProfiles = callsOf(profilesBackend);
    invoke.mockClear();
    const out = () => screen.getByTestId('watch-preview-text').textContent!.replace(/\s+/g, ' ');

    expect(out()).not.toContain('Preview only —');
    await fireEvent.click(screen.getByRole('button', { name: 'Simulate a change' }));
    expect(out()).toContain('Waiting for quiet · 5 s');
    expect(out()).toContain('Any new change resets the countdown to 5 s');

    vi.advanceTimersByTime(3000);
    await tick();
    expect(out()).toContain('Waiting for quiet · 2 s');

    // A new change resets the countdown to the full delay.
    await fireEvent.click(screen.getByRole('button', { name: 'Simulate a change' }));
    expect(out()).toContain('Waiting for quiet · 5 s');
    vi.advanceTimersByTime(4000);
    await tick();
    expect(out()).toContain('Waiting for quiet · 1 s');

    // Quiet at last: the preview ends with the text and nothing keeps ticking.
    vi.advanceTimersByTime(1000);
    await tick();
    expect(out()).toContain('Preview only — 2 changes batched into one run');
    expect(out()).not.toContain('Waiting for quiet');
    expect(vi.getTimerCount()).toBe(0);

    // A press after the end starts a fresh preview.
    await fireEvent.click(screen.getByRole('button', { name: 'Simulate a change' }));
    expect(out()).toContain('Waiting for quiet · 5 s');
    vi.advanceTimersByTime(5000);
    await tick();
    expect(out()).toContain('Preview only — 1 change batched into one run');

    // Nothing reached the backend, the profiles or the Tauri bridge.
    expect(callsOf(backend)).toEqual(before);
    expect(callsOf(profilesBackend)).toEqual(beforeProfiles);
    expect(invoke).not.toHaveBeenCalled();
    expect(backend.runNow).not.toHaveBeenCalled();
    expect(backend.approve).not.toHaveBeenCalled();
  });

  it('editing the idle delay drops a running preview', async () => {
    setup();
    await openWatchEditor();
    vi.useFakeTimers();
    await fireEvent.click(screen.getByRole('button', { name: 'Simulate a change' }));
    expect(screen.getByTestId('watch-preview-count').textContent).toContain('10 s');
    await fireEvent.click(screen.getByRole('button', { name: 'Longer idle delay' }));
    await tick();
    expect(screen.queryByTestId('watch-preview-count')).toBeNull();
    expect(vi.getTimerCount()).toBe(0);
  });
});

describe('Review of a watch automation (AP-3)', () => {
  it('shows the watch trigger and its burst and self-generated-change policies', async () => {
    setup();
    await openWatchEditor();
    await fireEvent.input(screen.getByRole('textbox', { name: 'Name' }), {
      target: { value: 'Photo watch' },
    });
    await fireEvent.click(await screen.findByRole('radio', { name: /Projects backup/ }));
    await typeInto(idle(), '30');
    const cont = screen.getByRole('button', { name: 'Continue to review' }) as HTMLButtonElement;
    await vi.waitFor(() => expect(cont.disabled).toBe(false));
    await fireEvent.click(cont);

    const review = await screen.findByTestId('automation-review');
    expect(review.textContent).toContain(
      'Watches the left folder · runs after 30 s without changes · runs when a folder comes back',
    );
    const policies = screen.getAllByTestId('watch-policy').map((li) => li.textContent);
    expect(policies.join(' ')).toContain('restarts the 30 s countdown');
    expect(policies.join(' ')).toContain('15 min at the latest');
    expect(policies.join(' ')).toContain('own runs are ignored');
    expect(policies.join(' ')).toContain('pending rerun');
    // A schedule's missed-run text does not appear for a watch.
    expect(review.textContent).not.toContain('If a run is missed');
  });
});
