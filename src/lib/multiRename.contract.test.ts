import { describe, expect, it } from 'vitest';
import cases from '../../contract-fixtures/multi-rename/cases.json';
import { DEFAULT_OPTIONS, buildPreview, executablePairs, type RenameOptions } from './multiRename';

// Shared with the Rust test `executor_realises_the_shared_preview_fixture`
// (src-tauri/src/rename_batch.rs): TS proves preview == expectedTargets, Rust
// proves the executor turns the same inputs into exactly those names.
describe('multi-rename preview matches the shared contract fixture', () => {
  for (const c of cases as {
    name: string;
    inputs: string[];
    rule: Partial<RenameOptions>;
    expectedTargets: string[];
  }[]) {
    it(c.name, () => {
      const preview = buildPreview(c.inputs, { ...DEFAULT_OPTIONS, ...c.rule });
      expect(preview.map((p) => p.target)).toEqual(c.expectedTargets);
      expect(preview.every((p) => !p.issue)).toBe(true);
      // Only changing entries become executor pairs, in selection order.
      const paths = c.inputs.map((n) => `/dir/${n}`);
      expect(executablePairs(paths, preview)).toEqual(
        c.inputs
          .map((n, i) => ({ from: `/dir/${n}`, to: c.expectedTargets[i] }))
          .filter((_, i) => c.inputs[i] !== c.expectedTargets[i]),
      );
    });
  }
});
