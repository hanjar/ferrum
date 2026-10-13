// FER-476: TS mirror of the Rust `indexer.status` / `indexer.setMode` intents
// (src-tauri/src/ai/contract.rs). Same shared cases as the Rust tests; legacy `parseIntent` keeps
// rejecting them. Also covers the offer rule and the app-built texts (no path, no model text).
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import cases from '../../../contract-fixtures/ai/indexer-cases.json';
import {
  INDEXER_SET_MODE_CAPABILITY,
  INDEXER_STATUS_CAPABILITY,
  parseIndexerSetModeRequest,
  parseIntent,
  parseOfferedIntent,
} from './contracts';
import {
  describeIndexerStatus,
  describeModeChange,
  INDEXER_CAPABILITY_GATES,
  INDEXER_SET_MODE_SCHEMA,
  INDEXER_STATUS_SCHEMA,
  modeChangeFailure,
  modeChangeSuccess,
  offeredIndexerCapabilityIds,
} from './indexerControl';
import { parseModelReply } from './provider';
import { IndexerError, UNAVAILABLE, type IndexerStatus } from '../ipc/indexer';

const OFFERED = [
  'archive.create',
  'directories.sync',
  INDEXER_STATUS_CAPABILITY,
  INDEXER_SET_MODE_CAPABILITY,
] as const;
const MUTATING_ONLY = ['archive.create', 'directories.sync'] as const;
const here = dirname(fileURLToPath(import.meta.url));
const rustContract = readFileSync(join(here, '../../../src-tauri/src/ai/contract.rs'), 'utf-8');
const rustProvider = readFileSync(join(here, '../../../src-tauri/src/ai/provider.rs'), 'utf-8');

describe('indexer contract (Rust parity)', () => {
  it('shared cases: accepted exactly when Rust accepts them, if offered', () => {
    expect(cases.length).toBeGreaterThanOrEqual(25);
    for (const c of cases) {
      const run = () => parseOfferedIntent(c.value, 'req_1', OFFERED);
      if (c.valid) expect(run().intent.capability, c.name).toMatch(/^indexer\./);
      else expect(run, c.name).toThrow();
    }
  });

  it('legacy parseIntent and a list without the indexer ids reject every case', () => {
    for (const c of cases) {
      expect(() => parseIntent(c.value, 'req_1'), c.name).toThrow();
      expect(() => parseOfferedIntent(c.value, 'req_1', MUTATING_ONLY), c.name).toThrow();
      expect(() => parseOfferedIntent(c.value, 'req_1', []), c.name).toThrow();
    }
  });

  it('offering one id does not open the other', () => {
    const status = {
      schemaVersion: 1,
      requestId: 'req_1',
      intent: { capability: 'indexer.status' },
    };
    const mode = {
      schemaVersion: 1,
      requestId: 'req_1',
      intent: { capability: 'indexer.setMode', mode: 'shared' },
    };
    expect(() => parseOfferedIntent(status, 'req_1', [INDEXER_SET_MODE_CAPABILITY])).toThrow();
    expect(() => parseOfferedIntent(mode, 'req_1', [INDEXER_STATUS_CAPABILITY])).toThrow();
  });

  it('the IPC-style request wire (intent minus capability) accepts exactly one mode', () => {
    expect(parseIndexerSetModeRequest({ mode: 'shared' })).toEqual({ mode: 'shared' });
    for (const bad of [
      {},
      { mode: 'turbo' },
      { mode: 'Shared' },
      { mode: null },
      { mode: 'shared', confirmed: true },
      { mode: 'shared', path: 'C:/' },
      { capability: 'indexer.setMode', mode: 'shared' },
      null,
      [],
    ])
      expect(() => parseIndexerSetModeRequest(bad), JSON.stringify(bad)).toThrow();
  });

  it('ids equal the Rust constants and prompts', () => {
    expect(rustContract).toContain(
      `pub const INDEXER_STATUS_CAPABILITY: &str = "${INDEXER_STATUS_CAPABILITY}";`,
    );
    expect(rustContract).toContain(
      `pub const INDEXER_SET_MODE_CAPABILITY: &str = "${INDEXER_SET_MODE_CAPABILITY}";`,
    );
    expect(rustProvider).toContain(`("${INDEXER_STATUS_CAPABILITY}", INDEXER_STATUS_PROMPT)`);
    expect(rustProvider).toContain(`("${INDEXER_SET_MODE_CAPABILITY}", INDEXER_SET_MODE_PROMPT)`);
  });

  it('model-facing schema examples parse and the schemas are closed', () => {
    for (const schema of [INDEXER_STATUS_SCHEMA, INDEXER_SET_MODE_SCHEMA]) {
      expect(schema.additionalProperties).toBe(false);
      expect(schema.examples.length).toBeGreaterThanOrEqual(1);
      for (const intent of schema.examples) {
        expect(() =>
          parseOfferedIntent({ schemaVersion: 1, requestId: 'req_1', intent }, 'req_1', OFFERED),
        ).not.toThrow();
      }
    }
    expect(INDEXER_SET_MODE_SCHEMA.required).toEqual(['capability', 'mode']);
    expect(Object.keys(INDEXER_STATUS_SCHEMA.properties)).toEqual(['capability']);
  });
});

describe('parseModelReply: indexer', () => {
  const wrap = (intent: object) => ({ schemaVersion: 1, requestId: 'req_1', intent });

  it('status becomes an indexerStatus event only when offered', () => {
    const reply = wrap({ capability: 'indexer.status' });
    expect(parseModelReply(reply, 'req_1', OFFERED)).toEqual({ kind: 'indexerStatus' });
    expect(() => parseModelReply(reply, 'req_1', MUTATING_ONLY)).toThrow();
    expect(() => parseModelReply(reply, 'req_1', undefined)).toThrow();
  });

  it('setMode becomes an indexerSetMode event carrying only the mode', () => {
    const reply = wrap({ capability: 'indexer.setMode', mode: 'high_performance' });
    expect(parseModelReply(reply, 'req_1', OFFERED)).toEqual({
      kind: 'indexerSetMode',
      request: { mode: 'high_performance' },
    });
    expect(() => parseModelReply(reply, 'req_1', MUTATING_ONLY)).toThrow();
  });

  it('rejects an invalid request even when offered', () => {
    expect(() =>
      parseModelReply(wrap({ capability: 'indexer.setMode', mode: 'turbo' }), 'req_1', OFFERED),
    ).toThrow();
    expect(() =>
      parseModelReply(
        wrap({ capability: 'indexer.setMode', mode: 'shared', confirmed: true }),
        'req_1',
        OFFERED,
      ),
    ).toThrow();
  });
});

describe('indexer offer rule', () => {
  it('is offered on the desktop app only, both ids together', () => {
    expect(offeredIndexerCapabilityIds({ desktopAvailable: true })).toEqual([
      'indexer.status',
      'indexer.setMode',
    ]);
    expect(offeredIndexerCapabilityIds({ desktopAvailable: false })).toEqual([]);
    expect(INDEXER_CAPABILITY_GATES['indexer.status'].status).toBe('ready');
    expect(INDEXER_CAPABILITY_GATES['indexer.setMode'].status).toBe('ready');
    expect(
      offeredIndexerCapabilityIds({
        desktopAvailable: true,
        gates: { 'indexer.setMode': { status: 'pending', reason: 'x' } },
      }),
    ).toEqual(['indexer.status']);
  });
});

const live = (over: Partial<IndexerStatus> = {}): IndexerStatus => ({
  ...UNAVAILABLE,
  installed: true,
  scmState: 'running',
  reachable: true,
  version: '1.0.0',
  mode: 'shared',
  state: 'running',
  index: 'ready',
  problem: null,
  volumes: [],
  ...over,
});

describe('describeIndexerStatus', () => {
  it('answers each readiness state honestly', () => {
    expect(describeIndexerStatus({ ...UNAVAILABLE, problem: null }).readiness).toBe(
      'not_installed',
    );
    expect(describeIndexerStatus(live({ reachable: false, mode: null })).readiness).toBe('stopped');
    expect(describeIndexerStatus(live({ index: 'building' })).readiness).toBe('building');
    expect(describeIndexerStatus(live()).readiness).toBe('ready');
    expect(describeIndexerStatus(live({ index: 'error' })).readiness).toBe('error');
    // FER-476 review H1: degraded (queries off) is reported as such.
    const d = describeIndexerStatus(live({ degraded: true }));
    expect(d.readiness).toBe('degraded');
    expect(d.summary).toContain('degraded');
    expect(d.summary).toContain('scan biasa');
    expect(describeIndexerStatus(live()).summary).toContain('siap');
    expect(describeIndexerStatus({ ...UNAVAILABLE, problem: null }).summary).toContain(
      'belum terpasang',
    );
  });

  it('FER-477: folder sizes only in High performance; incremental sync once ready', () => {
    const hp = describeIndexerStatus(live({ mode: 'high_performance' }));
    expect(hp.features).toEqual({ folderSizes: true, incrementalSync: true });
    expect(hp.summary).toContain('Ukuran folder dari indeks tersedia');
    const shared = describeIndexerStatus(live());
    expect(shared.features).toEqual({ folderSizes: false, incrementalSync: true });
    expect(shared.summary).toContain('tidak tersedia di mode Bersama');
    for (const s of [
      live({ index: 'building', mode: 'high_performance' }),
      live({ degraded: true, mode: 'high_performance' }),
      live({ reachable: false, mode: null }),
    ]) {
      expect(describeIndexerStatus(s).features).toEqual({
        folderSizes: false,
        incrementalSync: false,
      });
    }
    // The High performance card says that folder sizes come with it.
    expect(
      describeModeChange('high_performance', 'shared', false).consequences.join(' '),
    ).toContain('Ukuran folder');
    expect(describeModeChange('shared', 'high_performance', false).needsAck).toBe(false);
  });

  it('FER-480: a squatted pipe name is named as such, with the PID', () => {
    const v = describeIndexerStatus(
      live({ reachable: false, mode: null, problem: 'squatted', squatterPid: 4242 }),
    );
    expect(v.readiness).toBe('stopped');
    expect(v.summary).toBe(
      'Nama pipe FerrumIndexer dipakai proses lain (PID 4242) — indeks tidak dipakai, scan biasa aktif.',
    );
  });

  it('shows per-volume progress as percent (permille / 10) and the average while building', () => {
    const v = describeIndexerStatus(
      live({
        index: 'building',
        volumes: [
          {
            letter: 'C',
            state: 'building',
            error: null,
            entries: 10,
            progressPermille: 425,
            live: false,
          },
          {
            letter: 'D',
            state: 'ready',
            error: null,
            entries: 99,
            progressPermille: 1000,
            live: true,
          },
        ],
      }),
    );
    expect(v.volumes.map((x) => [x.letter, x.percent])).toEqual([
      ['C', 42],
      ['D', 100],
    ]);
    expect(v.summary).toContain('belum siap');
    expect(v.summary).toContain('71%');
  });

  it('reports paused work, the mode and failed drives (codes only)', () => {
    const v = describeIndexerStatus(
      live({
        indexPaused: true,
        mode: 'high_performance',
        volumes: [
          {
            letter: 'E',
            state: 'error',
            error: 'access_denied',
            entries: 0,
            progressPermille: 0,
            live: false,
          },
          {
            letter: 'C',
            state: 'ready',
            error: null,
            entries: 5,
            progressPermille: 1000,
            live: true,
          },
        ],
      }),
    );
    expect(v.paused).toBe(true);
    expect(v.mode).toBe('high_performance');
    expect(v.summary).toContain('dijeda');
    expect(v.summary).toContain('E: access_denied');
    expect(v.volumes[0].error).toBe('access_denied');
  });

  it('never carries a path: only letters, numbers, codes and app text', () => {
    const v = describeIndexerStatus(
      live({
        volumes: [
          {
            letter: 'C',
            state: 'error',
            error: 'C:\\Users\\x',
            entries: 1,
            progressPermille: 1,
            live: false,
          },
        ],
      }),
    );
    expect(v.volumes[0].error).toBe('internal');
    expect(JSON.stringify(v)).not.toContain('Users');
  });
});

describe('describeModeChange / results', () => {
  it('raising spells out the privacy consequence and needs the tick; lowering does not', () => {
    const up = describeModeChange('high_performance', 'shared', false);
    expect(up.needsAck).toBe(true);
    expect(up.ackLabel).toBe('Saya mengerti, PC ini hanya dipakai oleh saya.');
    const text = up.consequences.join(' ');
    expect(text).toContain('SEMUA akun');
    expect(text).toContain('satu orang');
    expect(text).toContain('UAC');
    const down = describeModeChange('shared', 'high_performance', true);
    expect(down.needsAck).toBe(false);
    expect(down.consequences.join(' ')).toContain('akun Windows-nya sendiri');
    expect(down.consequences.join(' ')).toContain('Profil akun lain tidak diindeks');
  });

  it('maps every IndexerError code to an Indonesian message', () => {
    for (const code of ['declined', 'not_running', 'refused', 'failed', 'unsupported']) {
      const m = modeChangeFailure(new IndexerError(code, 'C:\\secret raw message'));
      expect(m.code).toBe(code);
      expect(m.message).toContain('Mode tidak berubah');
      expect(m.message).not.toContain('secret');
    }
    expect(modeChangeFailure(new IndexerError('declined')).message).toContain('UAC');
    expect(modeChangeFailure(new Error('boom')).code).toBe('failed');
    expect(modeChangeFailure(new IndexerError('weird_code')).message).toContain('gagal');
  });

  it('success text tells whether anything changed', () => {
    expect(
      modeChangeSuccess({ mode: 'high_performance', changed: true, elevated: true }).message,
    ).toContain('administrator');
    expect(
      modeChangeSuccess({ mode: 'shared', changed: false, elevated: false }).message,
    ).toContain('tidak ada yang diubah');
  });
});
