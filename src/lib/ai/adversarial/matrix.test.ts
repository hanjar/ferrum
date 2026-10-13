// FER-223 / AI-13.1: baris matriks acceptance (spesifikasi bagian 12) yang bisa diuji di vitest dengan engine tiruan
// yang MENGHITUNG penulisan (mock `archive_create`). Semua lapisan di atas IPC nyata: ContextService, ScopeResolver,
// ContextFlow, registry, facade, adapter archive, runPack, ledger, executor. Peta lengkap baris -> bukti ada di
// docs/ai/verification.md.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { FsError } from '../../ipc/types';

const ipc = vi.hoisted(() => ({
  archiveCreate:
    vi.fn<
      (paths: string[], target: string, format: string, conflict?: string) => Promise<string>
    >(),
  compareDirs: vi.fn(),
  enqueueCopy: vi.fn(),
}));
vi.mock('../../ipc/archive', () => ({ archiveCreate: ipc.archiveCreate }));
vi.mock('../../ipc/compare', () => ({ compareDirs: ipc.compareDirs, compareFiles: vi.fn() }));
vi.mock('../../ipc/mutations', () => ({ enqueueCopy: ipc.enqueueCopy }));

import { entry } from '../contextTestKit';
import { MemoryExecutionLedger } from '../executorLedger';
import { LEFT, Gate, archiveIntent, buildHarness, dump, files } from './harness';

beforeEach(() => {
  ipc.archiveCreate.mockReset();
  ipc.compareDirs.mockReset();
  ipc.enqueueCopy.mockReset();
  ipc.archiveCreate.mockImplementation(async (_paths, target) => target);
});

const noWrites = () => {
  expect(ipc.archiveCreate).not.toHaveBeenCalled();
  expect(ipc.enqueueCopy).not.toHaveBeenCalled();
};

describe('matrix: ZIP semua file di panel kiri', () => {
  it('payload engine == item preview, satu penulisan setelah satu persetujuan, sukses dari hasil engine', async () => {
    const h = buildHarness({
      left: [
        ...files(LEFT, ['a.txt', 'b.txt']),
        entry('sub', { path: `${LEFT}\\sub`, isDir: true }),
      ],
    });
    const { runId } = h.submit();
    await h.waitState(runId, 'awaitingApproval');
    noWrites();
    const rows = await h.ex.listItems(runId, 0, 50);
    const previewCount = h.lastOf(runId).plan?.preview.itemCount;
    h.ex.approve(runId);
    await h.waitState(runId, 'succeeded');
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
    const [paths, target, format, conflict] = ipc.archiveCreate.mock.calls[0];
    expect(paths).toEqual([`${LEFT}\\a.txt`, `${LEFT}\\b.txt`, `${LEFT}\\sub`]);
    expect(paths.length).toBe(previewCount);
    expect(rows.total).toBe(paths.length);
    expect(target).toBe(`${LEFT}\\Invoice.zip`);
    expect(format).toBe('zip');
    expect(conflict).toBe('skip'); // chat tidak pernah meminta overwrite
    expect(h.lastOf(runId).receipt).toMatchObject({ state: 'completed', completed: 3, failed: 0 });
  });

  it('"yang dipilih" dengan selection kosong: pesan input kosong, tidak memilih semua, nol penulisan', async () => {
    const h = buildHarness();
    h.host.left.selected = [];
    h.provider.script = (r) => ({
      kind: 'intent',
      intent: archiveIntent(r.requestId, { selector: { kind: 'selected' } }),
    });
    const { runId } = h.submit('arsipkan yang dipilih');
    await h.waitState(runId, 'needsInput');
    expect(h.lastOf(runId).errorMessage).toBeTruthy();
    expect(h.updates.some((u) => u.state === 'awaitingApproval' || u.state === 'preview')).toBe(
      false,
    );
    h.ex.approve(runId); // tombol tidak ada, tetapi panggilan paksa pun tidak boleh menulis
    await Promise.resolve();
    noWrites();
  });

  it('panel 100k entri: model hanya menerima hitungan, tanpa satu nama atau path pun', async () => {
    const big = Array.from({ length: 100_000 }, (_, i) =>
      entry(`file-${i}.dat`, { path: `${LEFT}\\file-${i}.dat` }),
    );
    const h = buildHarness({ left: big });
    h.submit();
    await vi.waitFor(() => expect(h.provider.requests).toHaveLength(1));
    const sent = dump(h.provider.requests[0]);
    expect(sent.length).toBeLessThan(2_000);
    expect(sent).not.toContain('file-');
    expect(sent).not.toContain(LEFT);
    expect(h.provider.requests[0].context.leftCount).toBe(100_000);
  }, 60_000);

  it('batas kontrak saat ini: 10.000 item lolos end-to-end (scope = seluruh listing, bukan jendela virtual)', async () => {
    const n = 10_000;
    const h = buildHarness({
      left: Array.from({ length: n }, (_, i) => entry(`f${i}`, { path: `${LEFT}\f${i}` })),
    });
    const { runId } = h.submit();
    await h.waitState(runId, 'awaitingApproval');
    expect((await h.ex.listItems(runId, 0, 20)).total).toBe(n);
    h.ex.approve(runId);
    await h.waitState(runId, 'succeeded');
    expect(ipc.archiveCreate.mock.calls[0][0]).toHaveLength(n);
  }, 60_000);

  // BUG-1 (dilaporkan, bukan diperbaiki): `preview.itemCount` dan `receipt.completed` dibatasi 10.000 di kontrak TS
  // (contracts.ts MAX_STEPS) dan Rust (contract.rs check_count), sehingga "arsipkan semua file" di folder dengan
  // >10.000 entri berakhir `failed` ("tidak lolos validasi kontrak"), bukan preview. Aman (nol penulisan) tetapi
  // melanggar baris matriks 100k. `it.fails` lulus selama bug ada dan akan merah ketika batas diperbaiki: ganti ke `it`.
  it('100k entri mencapai preview dan sukses (BUG-1 diperbaiki: batas hitungan item 1.000.000)', async () => {
    const big = Array.from({ length: 100_000 }, (_, i) =>
      entry(`file-${i}.dat`, { path: `${LEFT}\\file-${i}.dat` }),
    );
    const h = buildHarness({ left: big });
    const { runId } = h.submit();
    await h.waitState(runId, 'awaitingApproval');
    h.ex.approve(runId);
    await h.waitState(runId, 'succeeded');
  }, 60_000);

  it('filter aktif + hidden off: scope dan pengecualian terlihat di preview; anggota ZIP == preview', async () => {
    const h = buildHarness({
      left: [
        ...files(LEFT, ['laporan.pdf', 'catatan.txt']),
        entry('.rahasia.pdf', { path: `${LEFT}\\.rahasia.pdf`, hidden: true }),
      ],
    });
    h.host.left.filter = { mode: 'all', pattern: '', query: '.pdf' };
    const { runId } = h.submit('zip semua pdf');
    await h.waitState(runId, 'awaitingApproval');
    const u = h.lastOf(runId);
    expect(u.scopeNote).toMatch(/Filter aktif/);
    expect(u.scopeNote).toMatch(/tersembunyi/);
    expect(u.plan?.preview.itemCount).toBe(1);
    h.ex.approve(runId);
    await h.waitState(runId, 'succeeded');
    expect(ipc.archiveCreate.mock.calls[0][0]).toEqual([`${LEFT}\\laporan.pdf`]);
  });

  it('hidden on: item tersembunyi ikut dan preview tidak lagi mengklaim pengecualian hidden', async () => {
    const h = buildHarness({
      left: [
        ...files(LEFT, ['a.txt']),
        entry('.env.local', { path: `${LEFT}\\.env.local`, hidden: true }),
      ],
    });
    h.host.left.showHidden = true;
    const { runId } = h.submit();
    await h.waitState(runId, 'awaitingApproval');
    expect(h.lastOf(runId).plan?.preview.itemCount).toBe(2);
    expect(h.lastOf(runId).scopeNote).not.toMatch(/tersembunyi tidak ikut/);
    h.ex.approve(runId);
    await h.waitState(runId, 'succeeded');
    expect(ipc.archiveCreate.mock.calls[0][0]).toContain(`${LEFT}\\.env.local`);
  });

  it('tujuan ZIP di folder sumber: output baru tidak pernah menjadi input; nama yang ada tidak dipakai', async () => {
    const h = buildHarness({ left: files(LEFT, ['a.txt', 'Invoice.zip']) });
    const { runId } = h.submit();
    await h.waitState(runId, 'awaitingApproval');
    h.ex.approve(runId);
    await h.waitState(runId, 'succeeded');
    const [paths, target] = ipc.archiveCreate.mock.calls[0];
    expect(target).toBe(`${LEFT}\\Invoice (2).zip`);
    expect(paths).not.toContain(target);
    expect(paths).toContain(`${LEFT}\\Invoice.zip`); // ZIP lama = data pengguna, bukan output ini
  });

  it('nama ZIP muncul setelah preview (tidak terlihat panel): engine menolak (skip), lama utuh, hasil tidak "berhasil"', async () => {
    ipc.archiveCreate.mockRejectedValue(new FsError({ code: 'alreadyExists', message: 'exists' }));
    const h = buildHarness();
    const { runId } = h.submit();
    await h.waitState(runId, 'awaitingApproval');
    h.ex.approve(runId);
    await vi.waitFor(() => expect(['stale', 'failed']).toContain(h.stateOf(runId)));
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
    expect(ipc.archiveCreate.mock.calls[0][3]).toBe('skip');
    expect(h.updates.some((u) => u.state === 'succeeded')).toBe(false);
  });
});

describe('matrix: konteks berubah saat model menjawab', () => {
  const changes: Array<[string, (h: ReturnType<typeof buildHarness>) => void]> = [
    ['navigasi panel', (h) => (h.host.left.listing = {})],
    ['swap panel', (h) => h.host.swap()],
    ['tab ditutup', (h) => (h.host.left.tab = {})],
    ['hidden di-toggle', (h) => (h.host.left.showHidden = true)],
  ];
  for (const [label, change] of changes) {
    it(`${label} selama planning: rencana ditahan (stale), target tidak berpindah, nol penulisan`, async () => {
      const h = buildHarness();
      h.provider.gate = new Gate();
      const { runId } = h.submit();
      await vi.waitFor(() => expect(h.provider.requests).toHaveLength(1));
      change(h);
      h.provider.gate.open();
      await h.waitState(runId, 'stale');
      h.ex.approve(runId);
      await Promise.resolve();
      noWrites();
    });
  }

  it('fokus panel saja berubah selama planning: tetap valid (bukan perubahan bermakna)', async () => {
    const h = buildHarness();
    h.provider.gate = new Gate();
    const { runId } = h.submit();
    await vi.waitFor(() => expect(h.provider.requests).toHaveLength(1));
    h.host.active = 'right';
    h.provider.gate.open();
    await h.waitState(runId, 'awaitingApproval');
  });
});

describe('matrix: provider timeout / cancel / respons terlambat', () => {
  it('cancel saat planning lalu respons terlambat: tidak ada plan, job, ledger, atau penulisan', async () => {
    const h = buildHarness();
    h.provider.gate = new Gate();
    const { runId } = h.submit();
    await vi.waitFor(() => expect(h.provider.requests).toHaveLength(1));
    expect(h.provider.requests[0].signal.aborted).toBe(false);
    h.ex.cancel(runId);
    expect(h.provider.requests[0].signal.aborted).toBe(true);
    h.provider.gate.open(); // provider mengabaikan abort dan tetap menjawab
    await new Promise((r) => setTimeout(r, 20));
    expect(h.stateOf(runId)).toBe('cancelled');
    expect(h.updates.filter((u) => u.runId === runId).map((u) => u.state)).toEqual(['cancelled']);
    expect(await h.ledger.unresolved()).toEqual([]);
    expect(await h.ledger.byExecution(`exe_${runId}`)).toBeUndefined();
    h.ex.approve(runId);
    await Promise.resolve();
    noWrites();
  });

  it('edit saat planning lalu respons terlambat dari rencana lama: dibuang, tidak ada plan/preview/penulisan', async () => {
    const h = buildHarness();
    h.provider.gate = new Gate();
    const { runId } = h.submit();
    await vi.waitFor(() => expect(h.provider.requests).toHaveLength(1));
    h.ex.edit(runId);
    expect(h.stateOf(runId)).toBe('needsInput');
    h.provider.gate.open();
    await new Promise((r) => setTimeout(r, 20));
    expect(h.updates.some((u) => u.state === 'awaitingApproval' || u.state === 'preview')).toBe(
      false,
    );
    h.ex.approve(runId);
    await Promise.resolve();
    noWrites();
  });

  it('provider timeout/error: pesan generik, status failed, tidak ada penulisan', async () => {
    const h = buildHarness();
    h.provider.error = new Error('request timed out after 30000ms');
    const { runId } = h.submit();
    await h.waitState(runId, 'failed');
    noWrites();
  });
});

describe('matrix: double dispatch, reconnect, restart', () => {
  it('klik Jalankan berulang + submit ulang run yang sama: satu eksekusi, satu panggilan engine', async () => {
    const h = buildHarness();
    const { runId, requestId } = h.submit();
    await h.waitState(runId, 'awaitingApproval');
    for (let i = 0; i < 5; i++) h.ex.approve(runId);
    h.ex.submit({ requestId, text: 'ulang', runId } as never); // reconnect UI: submit ulang dengan runId sama
    await h.waitState(runId, 'succeeded');
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
    expect(h.provider.requests).toHaveLength(1);
    expect(h.lastOf(runId).receipt?.jobIds).toEqual([]);
  });

  it('executor kedua (UI tersambung ulang) tidak dapat menyetujui run milik executor pertama', async () => {
    const h = buildHarness();
    const { runId } = h.submit();
    await h.waitState(runId, 'awaitingApproval');
    const ex2 = h.make();
    ex2.approve(runId); // run tidak dikenal oleh instance ini
    await Promise.resolve();
    noWrites();
  });

  it('permintaan KEDUA yang identik sebelum panel refresh mencapai engine lagi, tetapi hanya dengan conflict "skip"', async () => {
    // Temuan jujur: digest plan memuat requestId, jadi ledger tidak dedupe lintas request. Perlindungan dari
    // menimpa ada di backend (FER-411 `skip`); tes ini mengunci perlindungan itu.
    const h = buildHarness();
    const first = h.submit();
    await h.waitState(first.runId, 'awaitingApproval');
    h.ex.approve(first.runId);
    await h.waitState(first.runId, 'succeeded');
    const second = h.submit();
    await h.waitState(second.runId, 'awaitingApproval');
    h.ex.approve(second.runId);
    await h.waitState(second.runId, 'succeeded');
    expect(ipc.archiveCreate.mock.calls.map((c) => c[3])).toEqual(['skip', 'skip']);
  });

  it('proses mati setelah reserve sebelum receipt: restart = unknownOutcome, tanpa auto-replay', async () => {
    ipc.archiveCreate.mockImplementation(() => new Promise<string>(() => undefined)); // engine tak pernah kembali
    const h = buildHarness();
    const { runId } = h.submit();
    await h.waitState(runId, 'awaitingApproval');
    h.ex.approve(runId);
    await vi.waitFor(() => expect(ipc.archiveCreate).toHaveBeenCalledTimes(1));
    const journal = (h.ledger as MemoryExecutionLedger).persisted();
    expect(journal.entries.map((e) => e.state)).toEqual(['reserved']);

    const restarted = buildHarness({ ledger: MemoryExecutionLedger.restart(journal) });
    const unresolved = await restarted.ex.recover();
    expect(unresolved).toHaveLength(1);
    expect(unresolved[0].state).toBe('unknownOutcome');
    await new Promise((r) => setTimeout(r, 20));
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1); // restart tidak menjalankan ulang apa pun
    // rekonsiliasi eksplisit "tidak ditemukan" membebaskan plan; tetap tanpa penulisan otomatis
    await restarted.ex.reconcile(unresolved[0].key, { kind: 'notFound' });
    expect(await restarted.ex.recover()).toEqual([]);
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
  });
});

describe('matrix: izin ditolak / disk penuh di tengah Pack', () => {
  // Karakterisasi perilaku saat ini (LIMIT-5, dilaporkan): kegagalan engine apa pun setelah reserve dipetakan ke
  // `unknownOutcome` (konservatif: tidak ada replay, tidak ada klaim sukses), bukan `failed` dengan sebab. Tes ini
  // mengunci bagian yang aman; bila pemetaan sebab ditambahkan, perbarui ekspektasi status saja.
  for (const code of ['permissionDenied', 'io', 'notFound'] as const) {
    it(`engine gagal (${code}): tidak sukses, tidak diulang otomatis, tanpa pesan mentah engine`, async () => {
      ipc.archiveCreate.mockRejectedValue(
        new FsError({ code, message: `engine says: D:\\secret\\path ${code}` }),
      );
      const h = buildHarness();
      const { runId } = h.submit();
      await h.waitState(runId, 'awaitingApproval');
      h.ex.approve(runId);
      await vi.waitFor(() =>
        expect(['failed', 'unknownOutcome', 'partial']).toContain(h.stateOf(runId)),
      );
      expect(h.updates.some((u) => u.state === 'succeeded')).toBe(false);
      expect(dump(h.updates)).not.toContain('secret');
      await new Promise((r) => setTimeout(r, 30));
      expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
    });
  }
});

describe('matrix: capability pending', () => {
  it('sync ready (FER-168): provider ditawari kedua capability', async () => {
    const h = buildHarness();
    h.provider.script = (r) => ({
      kind: 'intent',
      intent: {
        schemaVersion: 1,
        requestId: r.requestId,
        intent: {
          capability: 'archive.create',
          sourcePane: 'left',
          selector: { kind: 'allVisible', entries: 'all' },
          format: 'zip',
          target: { kind: 'sourceDirectory' },
        },
      },
    });
    const { runId } = h.submit('zip kiri');
    await h.waitState(runId, 'awaitingApproval');
    expect(h.provider.requests[0].capabilities).toEqual(['archive.create', 'directories.sync']);
  });

  it('sync gate closed (revert path): provider tidak ditawari, intent sync diblokir jujur, tanpa compare/enqueue/fallback copy', async () => {
    const h = buildHarness({
      gates: {
        'directories.sync': {
          status: 'pending',
          reason: 'Sinkronisasi dua arah belum tersedia dari chat.',
        },
      },
    });
    h.provider.script = (r) => ({
      kind: 'intent',
      intent: {
        schemaVersion: 1,
        requestId: r.requestId,
        intent: {
          capability: 'directories.sync',
          leftPane: 'left',
          rightPane: 'right',
          mode: 'bidirectional',
        },
      },
    });
    const { runId } = h.submit('sinkronkan kedua panel');
    await h.waitState(runId, 'blockedCapability');
    expect(h.provider.requests[0].capabilities).toEqual(['archive.create']);
    expect(h.lastOf(runId).blockedReason).toMatch(/belum tersedia/i);
    expect(ipc.compareDirs).not.toHaveBeenCalled();
    noWrites();
  });
});
