import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import AutomationPage from './AutomationPage.svelte';
import { AutomationStore } from '../../stores/automationStore.svelte';
import { systemZone } from './automationModel';
import { toast } from '../../stores/toastStore.svelte';
import {
  fakeBackend,
  fakeProfiles,
  fixture,
  profileSummary,
  rows,
  scheduleDetail,
  type FakeBackend,
} from './automationTestKit';

function setup(
  over: Parameters<typeof fakeBackend>[0] = {},
  profiles = [profileSummary()],
  extra: { openSyncProfiles?: () => void; onclose?: () => void } = {},
) {
  const backend = fakeBackend(over);
  const store = new AutomationStore({
    backend,
    profiles: fakeProfiles(profiles),
    validateDelayMs: 0,
  });
  const view = render(AutomationPage, { store, ...extra });
  return { backend, store, view };
}

const input = (name: string) => screen.getByRole('textbox', { name }) as HTMLInputElement;

async function fillNewAutomation(name = 'Nightly projects backup') {
  await fireEvent.click(await screen.findByRole('button', { name: 'New automation' }));
  await fireEvent.input(input('Name'), { target: { value: name } });
  await fireEvent.click(await screen.findByRole('radio', { name: /Projects backup/ }));
}

const continueBtn = () =>
  screen.getByRole('button', { name: 'Continue to review' }) as HTMLButtonElement;

beforeEach(() => {
  toast.message = '';
});

describe('AutomationPage list (MH-1..MH-4)', () => {
  it('shows the tabs with counts, the attention badge and banner, and a row per automation', async () => {
    setup();
    await screen.findByText('Nightly projects backup');
    expect(screen.getByRole('tab', { name: /Automations \(3\)/ })).toBeTruthy();
    expect(screen.getByTestId('attention-badge').textContent).toBe('2 need attention');
    expect(screen.getByTestId('attention-banner').textContent).toMatch(
      /2 automations need you .* new runs are held/,
    );
    const photo = document.querySelector(
      '[data-automation="b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e"]',
    )!;
    expect(within(photo as HTMLElement).getByText('Needs review')).toBeTruthy();
    expect(
      within(photo as HTMLElement).getByRole('button', { name: 'Review changes' }),
    ).toBeTruthy();
    // Row facts: profile with approved rev, trigger, runtime note, last result with counts.
    const first = document.querySelector(
      '[data-automation="7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d"]',
    )!;
    expect(first.textContent).toContain('Projects backup · rev 3');
    expect(first.textContent).toContain('Daily at 02:00 (Asia/Jakarta)');
    expect(first.textContent).toContain('Only while open');
    expect(first.textContent).toContain('6 of 6');
  });

  it('shows a one-automation banner with its name', async () => {
    setup({ list: vi.fn(async () => [rows()[1]]) });
    const banner = await screen.findByTestId('attention-banner');
    expect(banner.textContent).toMatch(
      /1 automation needs you — Photo watch — new runs are held until reviewed\./,
    );
  });

  it('the banner link opens the filtered run history', async () => {
    const { backend } = setup();
    await fireEvent.click(await screen.findByRole('button', { name: 'Show in run history' }));
    await waitFor(() =>
      expect(backend.history).toHaveBeenCalledWith({ filter: 'attention', limit: 50 }),
    );
    expect(await screen.findByText('#1043')).toBeTruthy();
  });

  it('draws the queue strip from running, queued and pending rows', async () => {
    const list = rows();
    list[0] = { ...list[0], status: { kind: 'running', label: 'Running 3/6' } };
    setup({ list: vi.fn(async () => list) });
    const strip = await screen.findByTestId('queue-strip');
    expect(strip.textContent).toContain('Running 3/6');
  });

  it('Run now is dimmed with the backend reason when the row cannot run, otherwise it asks the backend', async () => {
    const { backend } = setup({
      runNow: vi.fn(async () => fixture('automation-run-now-queued.json') as never),
    });
    await screen.findByText('Nightly projects backup');
    const buttons = screen.getAllByRole('button', { name: /^Run now/ }) as HTMLButtonElement[];
    expect(buttons.length).toBe(3);
    const list = rows();
    list.forEach((r, i) => {
      expect(buttons[i].disabled).toBe(!r.canRunNow);
      if (!r.canRunNow) expect(buttons[i].title).toBe(r.runNowReason);
    });
    for (const b of buttons) await fireEvent.click(b);
    expect(backend.runNow).toHaveBeenCalledTimes(list.filter((r) => r.canRunNow).length);
    expect(backend.runNow).toHaveBeenCalledWith(list.find((r) => r.canRunNow)!.id);
    expect(backend.retry).not.toHaveBeenCalled();
    await waitFor(() => expect(toast.message).toBe('Queued — starts shortly'));
  });

  it('the Enabled switch asks the backend and reloads', async () => {
    const { backend } = setup();
    await screen.findByText('Nightly projects backup');
    await fireEvent.click(screen.getByRole('switch', { name: 'Enabled: Nightly projects backup' }));
    await waitFor(() => expect(backend.setEnabled).toHaveBeenCalledWith(rows()[0].id, false));
    await waitFor(() => expect(backend.list).toHaveBeenCalledTimes(2));
  });

  it('shows the empty state', async () => {
    setup({ list: vi.fn(async () => []) });
    expect(await screen.findByTestId('automation-empty')).toBeTruthy();
  });
});

describe('AutomationEditor (AP-2, SC-1, SC-2.3, RT-1)', () => {
  it('without profiles it says so and offers to create one', async () => {
    const openSyncProfiles = vi.fn();
    setup({}, [], { openSyncProfiles });
    await fireEvent.click(await screen.findByRole('button', { name: 'New automation' }));
    expect(screen.getByText('Automations need a saved sync profile')).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Create a sync profile ↗' }));
    expect(openSyncProfiles).toHaveBeenCalled();
    expect(continueBtn().disabled).toBe(true);
  });

  it('lists the exact validation messages and keeps Continue disabled while any exist', async () => {
    setup();
    await fireEvent.click(await screen.findByRole('button', { name: 'New automation' }));
    const errors = await screen.findByTestId('editor-errors');
    expect(errors.textContent).toContain('Enter a name');
    expect(continueBtn().disabled).toBe(true);

    await fireEvent.input(input('Name'), { target: { value: 'Nightly' } });
    await fireEvent.click(await screen.findByRole('radio', { name: /Projects backup/ }));
    await waitFor(() => expect(continueBtn().disabled).toBe(false));
    expect(screen.queryByTestId('editor-errors')).toBeNull();

    // SC-1.1: weekly with no weekday.
    await fireEvent.click(screen.getByRole('radio', { name: 'Weekly' }));
    expect((await screen.findByTestId('editor-errors')).textContent).toContain(
      'Pick at least one weekday',
    );
    expect(continueBtn().disabled).toBe(true);
    await fireEvent.click(screen.getByRole('button', { name: 'Monday' }));
    await waitFor(() => expect(continueBtn().disabled).toBe(false));

    // SC-1.1: an empty or impossible time.
    await fireEvent.input(screen.getByLabelText('Time'), { target: { value: '' } });
    expect((await screen.findByTestId('editor-errors')).textContent).toContain(
      'Enter a valid time',
    );
    expect(continueBtn().disabled).toBe(true);
  });

  it('shows a Once time that has passed with the exact text', async () => {
    setup();
    await fillNewAutomation();
    await fireEvent.click(screen.getByRole('radio', { name: 'Once' }));
    const date = screen.getByLabelText('Date') as HTMLInputElement;
    const now = new Date();
    const today = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
    // Pin the schedule zone to the system zone so "today" is unambiguous.
    await fireEvent.input(screen.getByLabelText('Time zone'), { target: { value: systemZone() } });
    await fireEvent.input(date, { target: { value: today } });
    await fireEvent.input(screen.getByLabelText('Time'), { target: { value: '00:00' } });
    const text = (await screen.findByTestId('editor-errors')).textContent;
    expect(text).toContain('That time has already passed — pick a later date or time');
  });

  it('shows the next 3 runs, relative times and the system-zone equivalent line (SC-1.2)', async () => {
    const base = Date.now();
    const sys = systemZone();
    const tz = sys === 'Asia/Jakarta' ? 'UTC' : 'Asia/Jakarta';
    const next = [0, 1, 2].map((i) => ({
      atMs: base + (9 * 60 + 15) * 60000 + 20000 + i * 86400000,
      label: `Run ${i + 1}, 02:00 zone`,
    }));
    const validate = vi.fn(async () => ({
      ...(fixture('automation-validate.json') as object),
      errors: [],
      checks: [],
      next,
    }));
    setup({ validate: validate as never });
    await fillNewAutomation();
    await fireEvent.input(screen.getByLabelText('Time zone'), { target: { value: tz } });
    const box = await screen.findByTestId('next-runs');
    await waitFor(() => expect(within(box).getAllByRole('listitem')).toHaveLength(3));
    expect(box.textContent).toContain('Run 1, 02:00 zone');
    expect(box.textContent).toContain('in 9 h 15 min');
    expect(box.querySelectorAll('.eq')).toHaveLength(3);
    expect(box.querySelector('.eq')!.textContent).toMatch(/^= \w{3} \d{2}:\d{2} /);
    // Same zone as the system: no equivalent line.
    await fireEvent.input(screen.getByLabelText('Time zone'), { target: { value: sys } });
    await waitFor(() => expect(box.querySelectorAll('.eq')).toHaveLength(0));
  });

  it('writes the daylight-saving note for DST and non-DST zones (SC-2.3)', async () => {
    setup();
    await fillNewAutomation();
    await fireEvent.input(screen.getByLabelText('Time zone'), {
      target: { value: 'Asia/Jakarta' },
    });
    expect(screen.getByTestId('dst-note').textContent).toBe(
      'Asia/Jakarta has no daylight-saving changes. For zones that do: a nonexistent local time is skipped, a repeated one runs once.',
    );
    await fireEvent.input(screen.getByLabelText('Time zone'), {
      target: { value: 'Europe/Amsterdam' },
    });
    expect(screen.getByTestId('dst-note').textContent).toBe(
      "Daylight saving: a local time that doesn't exist is skipped; a local time that happens twice runs once.",
    );
  });

  it('shows the Background card disabled with the RT-1.1 reason', async () => {
    setup();
    await fireEvent.click(await screen.findByRole('button', { name: 'New automation' }));
    const bg = document.querySelector('[data-runtime="background"]') as HTMLButtonElement;
    expect(bg.disabled).toBe(true);
    expect(bg.getAttribute('aria-disabled')).toBe('true');
    expect(bg.textContent).toContain('Unavailable');
    expect(bg.textContent).toContain("The background runner isn't included in this Ferrum build.");
    const app = document.querySelector('[data-runtime="app"]') as HTMLButtonElement;
    expect(app.getAttribute('aria-checked')).toBe('true');
    expect(app.textContent).toContain('Available');
    await fireEvent.click(bg);
    expect(app.getAttribute('aria-checked')).toBe('true');
  });

  it('lists the readiness checks with icons and a label for each status', async () => {
    const validate = vi.fn(async () => ({
      errors: [],
      next: [],
      checks: [
        { id: 'profile', status: 'ok', text: 'The sync profile is ready.' },
        { id: 'roots', status: 'warn', text: 'A root is slow.' },
        { id: 'runtime', status: 'fail', text: 'Not ready.' },
      ],
    }));
    setup({ validate: validate as never });
    await fillNewAutomation();
    const ready = await screen.findByRole('region', { name: 'Readiness' });
    await waitFor(() => expect(within(ready).getAllByRole('listitem')).toHaveLength(3));
    expect(within(ready).getByRole('img', { name: 'OK' })).toBeTruthy();
    expect(within(ready).getByRole('img', { name: 'Warning' })).toBeTruthy();
    expect(within(ready).getByRole('img', { name: 'Failed' })).toBeTruthy();
  });

  it('asks before discarding unsaved changes', async () => {
    setup();
    await fillNewAutomation();
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.getByRole('alertdialog', { name: 'Discard changes' })).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Keep editing' }));
    expect(screen.getByTestId('automation-editor')).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Discard changes' }));
    expect(await screen.findByRole('tab', { name: /Automations/ })).toBeTruthy();
  });
});

describe('AutomationReview (AP-3..AP-6)', () => {
  async function toReview(over: Parameters<typeof fakeBackend>[0] = {}) {
    const ctx = setup(over);
    await fillNewAutomation();
    await waitFor(() => expect(continueBtn().disabled).toBe(false));
    await fireEvent.click(continueBtn());
    await screen.findByTestId('automation-review');
    return ctx;
  }
  const approveBtn = (name = 'Approve & enable') =>
    screen.getByRole('button', { name }) as HTMLButtonElement;

  it('keeps Approve disabled until a test passed, then sends only the draft and test id', async () => {
    const { backend } = await toReview();
    expect(approveBtn().disabled).toBe(true);
    expect(screen.getByText('Run the test to see the effect on your files.')).toBeTruthy();

    await fireEvent.click(screen.getByRole('button', { name: 'Run test' }));
    const passed = await screen.findByTestId('test-passed');
    expect(passed.textContent).toContain('Test passed — no files were changed.');
    expect(passed.textContent).toContain(
      'The comparison found 6 operations; the real run recompares first.',
    );
    // The conflict warning and every step are listed.
    expect(screen.getByTestId('test-steps').querySelectorAll('li')).toHaveLength(7);
    expect(approveBtn().disabled).toBe(false);

    await fireEvent.click(approveBtn());
    await waitFor(() => expect(backend.approve).toHaveBeenCalledTimes(1));
    const arg = (backend.approve as FakeBackend['approve'] & ReturnType<typeof vi.fn>).mock
      .calls[0][0];
    expect(Object.keys(arg).sort()).toEqual(['draft', 'expectedConfigRev', 'id', 'testId']);
    expect(arg.testId).toBe('c0ffee00-1234-4abc-8def-0123456789ab');
    expect(arg.draft.name).toBe('Nightly projects backup');
    // Approved: back on the list, with the toast.
    await screen.findByRole('tab', { name: /Automations/ });
    expect(toast.message).toMatch(/enabled — background runs won't ask again for rev 3/);
  });

  it('shows the failed step and keeps Approve disabled', async () => {
    await toReview({ test: vi.fn(async () => fixture('automation-test-failed.json') as never) });
    await fireEvent.click(screen.getByRole('button', { name: 'Run test' }));
    const failed = await screen.findByTestId('test-failed');
    expect(failed.textContent).toBe(
      'C:\\Users\\you\\Projects cannot be read with your account. Check its permissions and test again.',
    );
    expect(screen.queryByTestId('test-passed')).toBeNull();
    expect(approveBtn().disabled).toBe(true);
    expect(screen.getByRole('button', { name: 'Run test again' })).toBeTruthy();
  });

  it('canceled keeps the user on the review screen', async () => {
    await toReview({
      approve: vi.fn(async () => fixture('automation-approve-canceled.json') as never),
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Run test' }));
    await screen.findByTestId('test-passed');
    await fireEvent.click(approveBtn());
    const notice = await screen.findByTestId('review-notice');
    expect(notice.textContent).toBe('Approval was canceled. Nothing was changed.');
    expect(screen.getByTestId('automation-review')).toBeTruthy();
    expect(approveBtn().disabled).toBe(false);
  });

  it('stale shows the exact text and disables Approve until a new test', async () => {
    await toReview({
      approve: vi.fn(async () => fixture('automation-approve-stale.json') as never),
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Run test' }));
    await screen.findByTestId('test-passed');
    await fireEvent.click(approveBtn());
    const notice = await screen.findByTestId('review-notice');
    expect(notice.textContent).toBe('The profile or automation changed — review again');
    expect(approveBtn().disabled).toBe(true);
    expect(screen.queryByTestId('test-passed')).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Run test' }));
    await screen.findByTestId('test-passed');
    expect(approveBtn().disabled).toBe(false);
  });

  it('Back returns to the editor with the form intact', async () => {
    await toReview();
    await fireEvent.click(screen.getByRole('button', { name: 'Back' }));
    expect(input('Name').value).toBe('Nightly projects backup');
  });

  it('re-review after a profile change lists from → to and offers Approve & resume', async () => {
    const { backend } = setup({ get: vi.fn(async () => fixture('automation-get.json') as never) });
    await fireEvent.click(await screen.findByRole('button', { name: 'Review changes' }));
    const changes = await screen.findByTestId('profile-changes');
    expect(changes.textContent).toContain('Profile changes since the approved revision');
    expect(changes.textContent).toContain('Delete policy: Recycle Bin → Permanent delete');
    expect(screen.getByTestId('review-reason').textContent).toContain(
      'Profile changed (rev 3 → 4) · held',
    );
    expect(approveBtn('Approve & resume').disabled).toBe(true);
    await fireEvent.click(screen.getByRole('button', { name: 'Run test' }));
    await screen.findByTestId('test-passed');
    await fireEvent.click(approveBtn('Approve & resume'));
    await waitFor(() => expect(backend.approve).toHaveBeenCalled());
    const arg = (backend.approve as ReturnType<typeof vi.fn>).mock.calls[0][0];
    expect(arg.id).toBe('b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e');
    expect(arg.expectedConfigRev).toBe(2);
  });

  it('labels an edited automation Approve & save', async () => {
    setup();
    await fireEvent.click((await screen.findAllByRole('button', { name: 'Edit' }))[0]);
    await waitFor(() => expect(continueBtn().disabled).toBe(false));
    await fireEvent.click(continueBtn());
    expect(await screen.findByRole('button', { name: 'Approve & save' })).toBeTruthy();
  });
});

describe('rework R1 (review of PR 35)', () => {
  const continueBtn2 = () =>
    screen.getByRole('button', { name: 'Continue to review' }) as HTMLButtonElement;

  it('B1: an edited schedule is reviewed as the DRAFT (Weekly), not the stored Daily text', async () => {
    setup();
    await fireEvent.click((await screen.findAllByRole('button', { name: 'Edit' }))[0]);
    await waitFor(() => expect(continueBtn2().disabled).toBe(false));
    await fireEvent.click(screen.getByRole('radio', { name: 'Weekly' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Monday' }));
    await waitFor(() => expect(continueBtn2().disabled).toBe(false));
    await fireEvent.click(continueBtn2());
    const review = await screen.findByTestId('automation-review');
    expect(review.textContent).toContain('Weekly on Mon at 02:00 (Asia/Jakarta)');
    expect(review.textContent).not.toContain('Daily at 02:00');
  });

  it('B2: a half-typed time zone does not throw, says so, and shows no DST claim', async () => {
    const errors: unknown[] = [];
    const onErr = (e: ErrorEvent) => errors.push(e.error);
    window.addEventListener('error', onErr);
    setup();
    await fillNewAutomation();
    await fireEvent.click(screen.getByRole('radio', { name: 'Once' }));
    await fireEvent.input(screen.getByLabelText('Time zone'), { target: { value: 'Asia/Ja' } });
    expect((await screen.findByTestId('editor-errors')).textContent).toContain(
      'Pick a valid time zone',
    );
    expect(screen.queryByTestId('dst-note')).toBeNull();
    expect(screen.getByLabelText('Date')).toBeTruthy();
    await fireEvent.input(screen.getByLabelText('Time zone'), { target: { value: '' } });
    await fireEvent.input(screen.getByLabelText('Time zone'), {
      target: { value: 'Europe/Amsterdam' },
    });
    expect(screen.getByTestId('dst-note')).toBeTruthy();
    window.removeEventListener('error', onErr);
    expect(errors).toEqual([]);
  });

  it('S2: Escape and Close do nothing while an approval is pending', async () => {
    let release!: (v: unknown) => void;
    const pending = new Promise((r) => (release = r));
    const { backend, store } = setup({ approve: vi.fn(() => pending as never) });
    await fillNewAutomation();
    await waitFor(() => expect(continueBtn2().disabled).toBe(false));
    await fireEvent.click(continueBtn2());
    await fireEvent.click(await screen.findByRole('button', { name: 'Run test' }));
    await screen.findByTestId('test-passed');
    await fireEvent.click(screen.getByRole('button', { name: 'Approve & enable' }));
    await waitFor(() => expect(backend.approve).toHaveBeenCalledTimes(1));
    await fireEvent.keyDown(screen.getByRole('dialog', { name: 'Automation' }), { key: 'Escape' });
    await fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(store.review).not.toBeNull();
    expect(store.view).toBe('review');
    release(fixture('automation-approve-canceled.json'));
    await screen.findByTestId('review-notice');
  });

  it('S6: the review shows the runtime of the draft, not a hardcoded one', async () => {
    const detail = fixture<Record<string, unknown>>('automation-get.json');
    (detail.policy as Record<string, unknown>).runtime = 'background';
    setup({ get: vi.fn(async () => detail as never) });
    await fireEvent.click(await screen.findByRole('button', { name: 'Review changes' }));
    const review = await screen.findByTestId('automation-review');
    expect(review.textContent).toContain('In the background');
    expect(review.textContent).not.toContain('While Ferrum is running');
  });

  it('R2-3: a stored Background runtime shows as the selected (unavailable) card and blocks Continue', async () => {
    const detail = scheduleDetail();
    detail.policy = { ...detail.policy, runtime: 'background' };
    setup({ get: vi.fn(async () => detail) });
    await fireEvent.click((await screen.findAllByRole('button', { name: 'Edit' }))[0]);
    const bg = (await waitFor(() => {
      const el = document.querySelector('[data-runtime="background"]');
      if (!el) throw new Error('no card');
      return el;
    })) as HTMLButtonElement;
    expect(bg.getAttribute('aria-checked')).toBe('true');
    expect(bg.disabled).toBe(true);
    expect(document.querySelector('[data-runtime="app"]')!.getAttribute('aria-checked')).toBe(
      'false',
    );
    expect((await screen.findByTestId('editor-errors')).textContent).toContain(
      "The background runner isn't included in this Ferrum build.",
    );
    expect(continueBtn2().disabled).toBe(true);
  });

  it('R2-1: after stale in edit mode the review offers Reload', async () => {
    const { store } = setup({
      approve: vi.fn(async () => fixture('automation-approve-stale.json') as never),
    });
    await fireEvent.click((await screen.findAllByRole('button', { name: 'Edit' }))[0]);
    await waitFor(() => expect(continueBtn2().disabled).toBe(false));
    await fireEvent.click(continueBtn2());
    await fireEvent.click(await screen.findByRole('button', { name: 'Run test' }));
    await screen.findByTestId('test-passed');
    await fireEvent.click(screen.getByRole('button', { name: 'Approve & save' }));
    await fireEvent.click(await screen.findByRole('button', { name: 'Reload' }));
    await screen.findByTestId('automation-editor');
    expect(store.view).toBe('editor');
  });
});
