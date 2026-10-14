import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  provesFullSync,
  reconcileBaseline,
  type ItemOutcome,
  type SyncBaseline,
} from './syncBaseline';
import type { CompareDirsReport } from './ipc/compare';
import type { SyncPlanItem } from './syncPlan';

interface SyncBaselineFixture {
  name: string;
  previous: SyncBaseline | null;
  after: CompareDirsReport;
  plan: SyncPlanItem[];
  outcomes: Record<string, ItemOutcome>;
  expectedBaseline: SyncBaseline;
  provesFullSync: boolean;
}

const here = dirname(fileURLToPath(import.meta.url));
const fixturesDir = join(here, '../../contract-fixtures/sync-baseline');

describe('syncBaseline contract fixtures parity', () => {
  const files = readdirSync(fixturesDir)
    .filter((f) => f.endsWith('.json'))
    .sort();

  for (const file of files) {
    const raw = readFileSync(join(fixturesDir, file), 'utf-8');
    const fixture: SyncBaselineFixture = JSON.parse(raw);

    it(`matches fixture: ${file} (${fixture.name})`, () => {
      const result = reconcileBaseline(
        fixture.previous,
        fixture.after,
        fixture.plan,
        fixture.outcomes,
      );

      expect(result).toEqual(fixture.expectedBaseline);
      expect(provesFullSync(fixture.after, result)).toBe(fixture.provesFullSync);
    });
  }
});
