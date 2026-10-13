/**
 * ACT-09 benchmark runner (FER-275/276).
 *
 * Runs the #[ignore] benchmarks of the activity store crate in release mode and
 * writes host spec + git SHA + raw output to docs/activity/benchmark-latest.txt:
 *   - tests/store.rs  benchmark_100k_entities_1m_episodes_query_p95   (ACT-09.3 query p50/p95)
 *   - tests/qa.rs     bench_startup_and_record_100k_entities_1m_episodes (open() + record() overhead)
 *   - tests/qa.rs     bench_startup_and_record_1m_entities_2m_episodes   (skipped with --skip-1m)
 *
 * usage: node scripts/bench-activity.mjs [--skip-1m] [--only-qa]
 * Deliberately never runs purge_before on large datasets (known >10 min gap).
 */
import { spawnSync, execSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import os from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const crate = join(root, 'src-tauri', 'src', 'activity', 'store');
const out = join(root, 'docs', 'activity', 'benchmark-latest.txt');
const args = new Set(process.argv.slice(2));

const sh = (cmd, cwd = root) => {
  try {
    return execSync(cmd, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
  } catch {
    return 'unknown';
  }
};

const runs = [];
if (!args.has('--only-qa')) {
  runs.push([
    'store query benchmark (tests/store.rs)',
    'store',
    'benchmark_100k_entities_1m_episodes_query_p95',
  ]);
}
runs.push([
  'startup + record() baseline 1k entities / 2k episodes (tests/qa.rs)',
  'qa',
  'bench_startup_and_record_1k_entities_baseline',
]);
runs.push([
  'startup + record() 100k entities / 1M episodes (tests/qa.rs)',
  'qa',
  'bench_startup_and_record_100k_entities_1m_episodes',
]);
if (!args.has('--skip-1m')) {
  runs.push([
    'startup + record() 1M entities / 2M episodes (tests/qa.rs)',
    'qa',
    'bench_startup_and_record_1m_entities_2m_episodes',
  ]);
}

const cpus = os.cpus();
const lines = [
  'Ferrum activity store benchmark',
  `date (UTC):   ${new Date().toISOString()}`,
  `git SHA:      ${sh('git rev-parse HEAD')} (${sh('git branch --show-current')})`,
  `dirty tree:   ${sh('git status --porcelain -- src-tauri/src/activity docs/activity scripts') ? 'yes (uncommitted activity changes)' : 'no'}`,
  `host:         ${os.type()} ${os.release()} ${os.arch()}, ${cpus.length} logical CPUs (${cpus[0]?.model?.trim()}), ${(os.totalmem() / 2 ** 30).toFixed(1)} GiB RAM`,
  `toolchain:    ${sh('rustc --version')}; ${sh('cargo --version')}`,
  'build:        cargo test --release --offline -j 2 (rusqlite bundled SQLite, on-disk temp DB, warm page cache)',
  '',
];

let failed = false;
for (const [title, test, name] of runs) {
  lines.push(`##### ${title}`);
  console.log(`running ${title} ...`);
  const started = Date.now();
  const r = spawnSync(
    'cargo',
    [
      'test',
      '--release',
      '--offline',
      '-j',
      '2',
      '--test',
      test,
      name,
      '--',
      '--ignored',
      '--nocapture',
      '--test-threads=1',
    ],
    {
      cwd: crate,
      encoding: 'utf8',
      maxBuffer: 64 * 1024 * 1024,
      shell: process.platform === 'win32',
    },
  );
  const text = `${r.stdout ?? ''}${r.status === 0 ? '' : `\n[stderr]\n${r.stderr ?? ''}`}`.trim();
  lines.push(text, `(wall ${((Date.now() - started) / 1000).toFixed(1)} s, exit ${r.status})`, '');
  if (r.status !== 0) failed = true;
}

writeFileSync(out, `${lines.join('\n')}\n`);
console.log(`wrote ${out}`);
process.exit(failed ? 1 : 0);
