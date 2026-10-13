# AI-09 — Percakapan lanjutan dan edit rencana (FER-166: FER-207..210)

Status: diimplementasikan di branch `feature/ai-h-followup`. Tanpa desktop smoke (lihat "Belum teruji").

## Ringkasan perilaku

Selama rencana belum masuk queue, pengguna dapat mengubahnya dengan pesan lanjutan. Setiap perubahan yang berhasil
menghasilkan **revision baru**: preview diperbarui, approval/handle/slot backend lama dicabut, riwayat intent
bertambah satu entri (immutable, maksimal 20). Setelah job `queued`/`running`, perubahan **ditolak** dengan pesan
jelas dan job aktif tidak disentuh.

| Ucapan                                                                         | Penanganan                                        | Hasil                                                            |
| ------------------------------------------------------------------------------ | ------------------------------------------------- | ---------------------------------------------------------------- |
| "namanya invoice-september.zip", "nama file x", "ganti nama jadi x"            | Lokal                                             | `outputName` (nama aman, satu token atau dalam tanda kutip)      |
| "taruh di kanan", "simpan ke kiri saja", "tujuannya kanan", "di folder sumber" | Lokal                                             | `target` pane kiri/kanan atau `sourceDirectory`                  |
| "hanya PDF", "pdf dan docx", "cuma .xlsx", "PDF saja"                          | Lokal bila ekstensi dikenali atau berawalan titik | `selector.extensions` (menggantikan filter sebelumnya)           |
| "hanya file", "tanpa folder"                                                   | Lokal                                             | `entries: 'filesOnly'` (tidak berlaku untuk selector `selected`) |
| "tanpa filter", "nama default"                                                 | Lokal                                             | hapus `extensions` / `outputName`                                |
| "batal", "batalkan", "cancel", "stop", "hentikan"                              | Lokal (executor/store)                            | membatalkan run itu saja                                         |
| Frasa lain ("pindahkan ke sisi seberang", "hanya foto")                        | Provider                                          | intent baru, tetap lewat `parseIntent` + allowlist               |

Pola lokal sengaja sempit: nama dengan banyak kata tanpa tanda kutip, ekstensi tanpa titik yang tidak dikenali
("hanya yang penting"), dan teks yang tidak cocok pola apa pun diserahkan ke provider, bukan ditebak.

### Keputusan rancangan

- **Provider hanya menerima ringkasan intent lama** (`capability`, `sourcePane`, `selector`, `format`, `target`, dan
  penanda `hasOutputName`), tanpa path, nama file, atau nama output. Ringkasan ditanam di `userText`
  (`composeProviderText`), sehingga kontrak IPC `PlanRequest` (Rust, `deny_unknown_fields`) tidak berubah. Bagian
  prompt backend (`provider.rs`) hanya menambah aturan follow-up dan deskripsi `extensions`.
- Karena nama output tidak dikirim, balasan provider tanpa `outputName` digabung secara lokal dengan nama
  sebelumnya (`mergeProviderIntent`); nama baru dari provider tetap diterima setelah divalidasi.
- Nama output yang tidak aman (`../x.zip`, `a:b.zip`, `CON.zip`, titik di awal, pemisah path, `;`) ditolak lokal
  dan **tidak** diteruskan ke provider. Filter wildcard/path (`*.p*`, `../x`, `pdf.exe`) ditolak. Lebih dari 8
  ekstensi ditolak.
- Edit gagal tidak "menempel": revision hanya dicatat bila `prepare` berhasil. Edit berikutnya dihitung dari
  revision terakhir yang berhasil.
- Provider boleh mengganti capability pada follow-up bebas; hasilnya tetap harus lolos `parseIntent`, ketersediaan
  capability, preview, dan approval. Edit lokal hanya berlaku untuk `archive.create` (sync tidak punya
  nama/tujuan/filter yang bisa diubah).
- Follow-up memakai **snapshot panel yang sama** dengan permintaan awal. Bila panel berpindah/berubah sejak itu,
  hasilnya `stale` (tidak mengalihkan target diam-diam); pengguna menyusun ulang.

## Kontrak

`selector.extensions` (opsional, kompatibel mundur) pada ketiga bentuk selector:

- 1..8 entri unik, tiap entri `^[a-z0-9]{1,16}$` (huruf kecil, tanpa titik, wildcard, atau pemisah path).
- TS: `contracts.ts` (`isSafeExtension`, `MAX_EXTENSIONS`); Rust: `contract.rs` (`safe_extensions`,
  `deny_unknown_fields` tetap). Fixture lintas bahasa: `contract-fixtures/ai/cases.json` (3 valid, 10 invalid).
- Resolver (`contextResolver.ts`) menerapkannya pada **listing penuh** (bukan baris ter-render): hanya file biasa
  (bukan folder/symlink) dengan ekstensi terakhir yang cocok, tanpa membedakan huruf besar/kecil. `.pdf` (dotfile)
  dan `a.` tidak punya ekstensi; `a.pdf.exe` berekstensi `exe`. `excluded.extension` menghitung item yang
  dilewati dan `scopeText` menyebut filter. Tidak ada kecocokan = `empty_scope` (bukan fallback ke semua item).

## Acuan eksplisit (FER-208)

`ChatExecutor.followUp(runId, { planId, revision }, text)`. Store mengambil acuan dari plan yang sedang tampil
(`RunView.plan.planId` + `RunView.revision`) atau dari argumen eksplisit `store.followUp(runId, text, ref)`.
Ditolak (`FollowUpResult.accepted === false`) dengan kode:

| Kode           | Arti                                                                                 |
| -------------- | ------------------------------------------------------------------------------------ |
| `unknown_run`  | run tidak dikenal                                                                    |
| `final`        | run sudah berakhir (succeeded/partial/failed/cancelled/stale/blocked/unknownOutcome) |
| `running`      | job sudah `queued`/`running` — edit tidak mengubah job aktif                         |
| `busy`         | rencana sedang disusun atau sedang disetujui                                         |
| `no_plan`      | belum ada plan (mis. run menunggu jawaban pertanyaan model)                          |
| `stale_ref`    | `planId`/`revision` bukan yang berlaku sekarang                                      |
| `invalid_edit` | teks/perubahan tidak aman atau tidak berlaku                                         |
| `limit`        | sudah 20 revisi                                                                      |

Pesan chat lama tidak pernah menjadi target: setelah run final, frasa perubahan ("namanya x.zip", "taruh di
kanan", "hanya PDF") di store menjadi catatan "rencana sudah berakhir", bukan permintaan baru dan bukan mutasi
hasil. Jawaban atas pertanyaan model (belum ada plan) tetap menjadi permintaan baru; kartu lama ditandai
`superseded`.

## Revision (FER-209)

`followUp` → `beginRevision`: `guard.revoke` + `facade.discard` (sekali, idempoten) untuk plan lama, `epoch++`
(`invalidate`), `AbortController` baru, `ApprovalManager.invalidate` (revision tiket naik, `bindingDigest` baru).
Lalu `buildPlan` (prepare → policy → register guard → preview). State: `awaitingApproval|needsInput → resolving`
(lokal) atau `→ planning → resolving` (provider). Approve dengan digest lama = `stale`. `IntentHistory`
(`followup.ts`) menyimpan salinan beku tiap revision; `RunUpdate.revision`/`revisionNotes` menampilkan "Revisi N: …"
di kartu. Respons provider yang terlambat setelah `cancel`/`edit` dibuang oleh pemeriksaan epoch yang sama dengan
permintaan awal. Kegagalan provider saat follow-up membawa run ke `needsInput` (pesan aman, tanpa rahasia), bukan
`failed`; rencana lama sudah dicabut sehingga pengguna mengulang perubahan.

## Job berjalan (FER-210)

- `queued`/`running`: semua frasa perubahan ditolak (`running`) dengan pesan: perubahan tidak mengubah job aktif;
  ketik "batal" lalu buat permintaan baru. Tidak ada run baru otomatis.
- "batal" selalu menargetkan run itu: `cancelJobs` hanya mengirim cancel ke job ID milik execution run tersebut
  (dari ledger). Run lain tidak tersentuh. Batal sebelum queue membuang plan dan handle.
- Pack ZIP saat ini selesai in-process (tanpa job ID/cancel); semantik "edit ditolak" tetap berlaku sebelum selesai.

## Berkas

Baru: `src/lib/ai/followup.ts`, `followup.test.ts`, `followupExecutor.test.ts`, `followupIntegration.test.ts`,
`chatStoreFollowUp.test.ts`, `src/lib/components/ai/ChatPanelFollowUp.test.ts`, `docs/ai/followup.md`.
Diubah: `contracts.ts`, `contextResolver.ts`, `executor.ts`, `executorState.ts`, `chatStore.svelte.ts`,
`ChatPanel.svelte`, `PlanCard.svelte`, `src-tauri/src/ai/contract.rs`, `provider.rs` (hanya prompt),
`contract-fixtures/ai/cases.json`, `contract-fixtures/ai/rust/tests/provider.rs`.

## Belum teruji

- Tidak ada desktop smoke: provider live, `backendGuard` nyata (register/authorize/revoke di Rust), dan Pack ZIP
  nyata tidak dijalankan; yang diuji adalah executor + facade + adapter archive + resolver nyata dengan batas IPC
  yang di-mock, plus `cargo test` fixture crate.
- Kualitas follow-up bebas (jalur provider) bergantung pada model; yang dijamin adalah validasi dan approval,
  bukan ketepatan tafsir.
- Tombol "Edit" lama (`store.edit`) tetap ada: ia mencabut plan dan mengembalikan run ke `needsInput`; follow-up
  setelahnya tetap terikat ke plan/revision terakhir.
