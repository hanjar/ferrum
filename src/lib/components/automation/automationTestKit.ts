/**
 * Test helpers for the Automation UI: contract fixtures from `contract-fixtures/automation` and a
 * fake backend whose every call is a `vi.fn` (so tests assert exactly what the UI sends).
 */
import { vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type {
  AutomationBackend,
  AutomationDetail,
  AutomationRow,
  AutomationTestResult,
  AutomationValidateResult,
} from '../../ipc/automation';
import type { ProfileDto, ProfileSummary, SyncProfilesBackend } from '../../ipc/syncProfiles';

const dir = join(
  dirname(fileURLToPath(import.meta.url)),
  '../../../../contract-fixtures/automation',
);
export const fixture = <T = Record<string, unknown>>(name: string): T =>
  JSON.parse(readFileSync(join(dir, name), 'utf-8')) as T;

export const PROFILE_ID = '3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f';

export const profileSummary = (over: Partial<ProfileSummary> = {}): ProfileSummary => ({
  id: PROFILE_ID,
  name: 'Projects backup',
  revision: 3,
  left: 'C:\\Users\\you\\Projects',
  right: 'E:\\Backups\\Projects',
  modeLabel: 'Update left → right',
  deleteLabel: 'Recycle Bin',
  readiness: { status: 'ready' },
  usedBy: 0,
  ...over,
});

export function rows(): AutomationRow[] {
  return fixture<AutomationRow[]>('automation-list.json');
}

/** The detail of the first (scheduled) fixture row. */
export function scheduleDetail(): AutomationDetail {
  const base = fixture<AutomationDetail>('automation-get.json');
  const row = rows()[0];
  const approved = fixture<{ automation: { policy: AutomationDetail['policy'] } }>(
    'automation-approve.json',
  );
  return { ...base, automation: row, policy: approved.automation.policy };
}

export type FakeBackend = AutomationBackend & {
  [K in keyof AutomationBackend]: ReturnType<typeof vi.fn> & AutomationBackend[K];
};

export function fakeBackend(over: Partial<AutomationBackend> = {}): FakeBackend {
  const b: AutomationBackend = {
    validate: vi.fn(async () => fixture<AutomationValidateResult>('automation-validate.json')),
    test: vi.fn(async () => fixture<AutomationTestResult>('automation-test.json')),
    approve: vi.fn(async () => fixture('automation-approve.json') as never),
    setEnabled: vi.fn(async () => ({ status: 'enabled' as const })),
    pause: vi.fn(async () => ({ status: 'paused' as const })),
    delete: vi.fn(async () => ({ status: 'deleted' as const })),
    list: vi.fn(async () => rows()),
    get: vi.fn(async () => scheduleDetail()),
    history: vi.fn(async () => fixture('automation-history.json') as never),
    runOps: vi.fn(async () => fixture('automation-run-ops.json') as never),
    events: vi.fn(async () => fixture('automation-events.json') as never),
    runNow: vi.fn(),
    stopRun: vi.fn(),
    recheck: vi.fn(),
    retry: vi.fn(),
    ...over,
  };
  return b as FakeBackend;
}

export function fakeProfiles(
  list: ProfileSummary[] = [profileSummary()],
  over: Partial<SyncProfilesBackend> = {},
): SyncProfilesBackend {
  return {
    list: vi.fn(async () => list),
    get: vi.fn(
      async () =>
        ({
          id: PROFILE_ID,
          name: 'Projects backup',
          revision: 4,
          changes: [
            {
              revision: 4,
              changedMs: 1,
              changes: [{ field: 'Delete policy', from: 'Recycle Bin', to: 'Permanent delete' }],
            },
          ],
        }) as unknown as ProfileDto,
    ),
    validate: vi.fn(),
    save: vi.fn(),
    delete: vi.fn(),
    preview: vi.fn(),
    run: vi.fn(),
    ...over,
  };
}
