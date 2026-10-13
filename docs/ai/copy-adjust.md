# `copy.adjust`: AI mengatur thread dan prioritas copy yang sedang berjalan (FER-471)

Capability kontrol (bukan plan). Pengguna berkata "copy-nya lebih cepat dong", "pakai 8 thread", "otomatis lagi",
"jalankan di latar belakang"; model memilih satu aksi, aplikasi menampilkan kartu konfirmasi, dan baru setelah
pengguna menekan **Terapkan** pengaturan job copy yang berjalan diubah.

## Intent

```json
{ "capability": "copy.adjust", "action": "increase" | "decrease", "by": 1 }
{ "capability": "copy.adjust", "action": "set", "threads": 8 }
{ "capability": "copy.adjust", "action": "auto" }
{ "capability": "copy.adjust", "action": "priority", "priority": "normal" | "background" }
```

- `by` opsional 1..8 (default 1); `threads` 1..16. Setiap aksi hanya membawa field miliknya; field lain, `null`,
  tipe salah, atau kunci tak dikenal ditolak (35 kasus bersama di `contract-fixtures/ai/copy-adjust-cases.json`,
  dipakai tes Rust dan TS).
- Tidak ada path, nama file, atau id job. **Aplikasi** yang memilih job (copy yang sedang berjalan; bila lebih dari satu,
  yang terbaru), jadi model tidak bisa menunjuk job lain.

## Alur

1. **Ditawarkan** hanya bila ada job copy yang berjalan: `AiExecutor.offered()` (port `CopyControlPort.available`) dan
   gate kedua di backend (`gate_control_capabilities` di `ai_plan_intent`, memakai `copy_pool::has_running()`).
   Pemeriksaan gagal = tidak ditawarkan (fail closed). Pemanggil lama tanpa port tidak melihatnya.
2. Model membalas intent; `parseModelReply` (TS) dan `parse_intent_offered` (Rust) memvalidasi ketat dan hanya bila
   capability ditawarkan.
3. Executor **tidak** memanggil prepare/policy/guard/ledger/queue. Ia membaca job yang berjalan, menghitung perubahan
   secara lokal (cermin `CopyControl::adjust`), lalu mengirim state `awaitingApproval` dengan `copyAdjust`
   (ringkasan, job, sebelum, sesudah). Semua teks dibuat aplikasi.
4. Tombol **Terapkan** = `approve(runId)` -> `copy_control_adjust` (perintah IPC yang sama dengan tombol +/- di dialog
   progres). Hasilnya diambil dari state yang dikonfirmasi backend. Tanpa klik tidak ada penulisan; **Batal** tidak
   menulis apa pun.
5. Permintaan yang tidak mengubah apa pun selesai tanpa konfirmasi dan tanpa penulisan. Job yang sudah selesai saat
   Terapkan ditekan = `failed` dengan pesan jujur, tidak ada yang diubah.

## Batas keamanan

- Efek hanya kinerja (jumlah worker, mode otomatis, prioritas OS). Tidak menyentuh file, tidak masuk ledger tulis,
  tidak membuat job antrean.
- Tidak ada jalur dari model ke IPC: hanya klik pada kartu yang memanggil `approve`. `copy_control_adjust` menolak id
  yang bukan job copy yang sedang berjalan.
- State machine plan tetap ketat; edge tambahan (`planning -> awaitingApproval|succeeded`,
  `awaitingApproval -> succeeded`) hanya berlaku untuk run dengan `control = true`
  (`canControlTransition` di `executorState.ts`).
- Prompt (Rust `COPY_ADJUST_PROMPT`) menyuruh model tidak menyebut job, path, atau id, dan prompt capability lama tetap
  byte-identik (golden test `provider_activity_tests.rs` tidak berubah).

## File

Rust: `src-tauri/src/ai/contract.rs` (`Intent::CopyAdjust`), `provider.rs` (prompt + `gate_control_capabilities`),
`commands.rs` (gate), tes `contract_copy_adjust_tests.rs`, `provider_copy_adjust_tests.rs`.
TS: `src/lib/ai/contracts.ts`, `copyAdjust.ts`, `provider.ts`, `executor.ts`, `executorState.ts`, `aiRuntime.ts`,
`src/lib/components/ai/CopyAdjustCard.svelte`; tes `copyAdjustContract.test.ts`, `executorCopyAdjust.test.ts`,
`CopyAdjustCard.test.ts`.
