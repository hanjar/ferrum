import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  applyDecisions,
  buildSyncPlan,
  describeBlockers,
  isPlanStale,
  planSummary,
  syncPlanBlockers,
  type SyncDecision,
  type SyncOptions,
  type SyncPlanItem,
  type PlanSummary,
} from './syncPlan';
import type { CompareDirsReport } from './ipc/compare';
import type { SyncBaseline } from './syncBaseline';

interface SyncPlanFixture {
  name: string;
  report: CompareDirsReport;
  options: SyncOptions;
  baseline: SyncBaseline | null;
  freshReport?: CompareDirsReport;
  decisions?: Record<string, SyncDecision>;
  expected: {
    items: SyncPlanItem[];
    summary: PlanSummary;
    blockers: string[];
    blockersDescription: string;
    isStale?: boolean;
    isStaleByContent?: boolean;
    resolvedItems?: SyncPlanItem[];
  };
}

const here = dirname(fileURLToPath(import.meta.url));
const fixturesDir = join(here, '../../contract-fixtures/sync-plan');

describe('syncPlan contract fixtures parity', () => {
  const files = readdirSync(fixturesDir)
    .filter((f) => f.endsWith('.json'))
    .sort();

  for (const file of files) {
    const raw = readFileSync(join(fixturesDir, file), 'utf-8');
    const fixture: SyncPlanFixture = JSON.parse(raw);

    it(`matches fixture: ${file} (${fixture.name})`, () => {
      const plan = buildSyncPlan(fixture.report, {
        ...fixture.options,
        baseline: fixture.baseline,
      });

      expect(plan).toEqual(fixture.expected.items);
      expect(planSummary(plan)).toEqual(fixture.expected.summary);
      expect(syncPlanBlockers(fixture.report)).toEqual(fixture.expected.blockers);
      expect(describeBlockers(fixture.report)).toBe(fixture.expected.blockersDescription);

      if (fixture.freshReport) {
        expect(isPlanStale(fixture.report, fixture.freshReport)).toBe(fixture.expected.isStale);
        if (fixture.expected.isStaleByContent !== undefined) {
          expect(isPlanStale(fixture.report, fixture.freshReport, { byContent: true })).toBe(
            fixture.expected.isStaleByContent,
          );
        }
      }

      if (fixture.decisions && fixture.expected.resolvedItems) {
        const resolved = applyDecisions(plan, fixture.decisions);
        expect(resolved).toEqual(fixture.expected.resolvedItems);
      }
    });
  }
});
