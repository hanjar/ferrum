# Ferrum 0.2.1 — changelog

Changes since 0.2.0 (`docs/release/v0.2.0/`). Windows 10/11 x64 only. Entries refer to public pull requests (`#n` on `hanjar/ferrum`).
Everything in the 0.2.0 [CHANGELOG](../v0.2.0/CHANGELOG.md) is still part of the product. This file lists only what is new or changed.

## Features

- **Sync profiles screen** (#25): a "Sync & Backup" sidebar section and a full-page **Sync profiles** screen on top of the existing profile backend. A list with readiness (Ready / Checking / Not ready, with the reason), revision, mode and delete labels; an editor with name, folders (path suggestions, "Use active pane" / "Use other pane"), direction (Update / Mirror / Two-way), delete policy, compare options, include/exclude filters with a live example, a delete guard and revision history; **Preview sync** (read-only dry run) and **Run now** with progress and result. Run now asks before any run that overwrites, or deletes under Recycle Bin or versioning; permanent deletes and turning the delete guard off use a native Windows confirmation. There is no "Browse..." folder button yet.

## Security

- **Sync respects protected folders and the Startup folder** (#31): sync profiles used to write to their folders without the checks that the file manager already applies. A root inside or equal to Ferrum's config, data or Activity folders, or the per-user or all-users Startup folder, is refused; a root that is written to may not contain such a folder either. A folder on a share that reaches this computer is refused. Before every create, copy, overwrite, delete and move into the versions tree, the planned path and then the final path of the open folder (this catches 8.3 aliases such as `STARTM~1`) are checked; a refused item fails on its own with reason `protected_folder` and nothing is written. Profiles saved earlier with such a folder become "not ready" and their runs fail until edited. Deleting inside Startup is also refused for sync.
- **Target re-check at overwrite, restore on failure** (#36): an in-place sync overwrite now opens the target without write sharing and compares size, time and file identity with the plan at the moment of the swap. A target that was edited, removed or replaced since the check is left as is and the item is reported as changed (`target_changed`); a program holding the file open for writing gives `sharing_violation`. With versioning, if the copy fails or is cancelled the old version is moved back by handle and never replaces a name that was taken meanwhile; if the restore itself fails the run stops, the profile needs attention and the old file stays in the versions folder. A staged file that cannot be published without losing the old one is reported as `backup_left`. Baselines are not advanced for a failed item, so Two-way treats it as a held conflict and never as a delete.
- **Remaining P3 hardening** (#29):
  - Startup folders: the file manager writes nothing into the Windows Startup folders (per user and all users). This is checked in the IPC gate for create, copy, move, extract, archive create, split, combine, encode, decode, checksum and selection, in the copy and move engines and in the extractor (also by final path), and covers the description sidecar (`comment_set`). Rename may not target Startup, something inside it, or an ancestor. Read, open and delete stay allowed.
  - Copy/move of sources named like devices (`nul`, `aux.c`): an existing plain file or folder with a reserved name is read through its `\\?\` form, and a move onto itself with Overwrite can no longer delete the source. Devices and missing entries are refused.
  - Recycle Bin for reserved names: the API rejects `\\?\` paths, so such an entry is refused up front with advice to delete it permanently; the entry is untouched.
  - An inherit-only ACE with object-inherit that grants another account a write right on the folder of the executable is untrusted (callback-aware variant; the strict rule is unchanged).
  - A symlink or junction among the folders above `Rar.exe` is refused (validate, set path and the re-check before each run).
  - Not done: a share that maps onto a protected folder through this machine's LAN IP or a hosts-file alias is not recognised (#29, residual risks).

## Fixes

- **Sync lease follow-ups** (#28): the lease release at page load no longer runs on the UI thread; an acquire that finishes after a reload releases its lease instead of leaving an orphan; `Escape` no longer closes the delete dialog while a lease-holding delete is running; docs no longer claim crash recovery that does not exist (after a renderer crash the leases stay until Ferrum exits); saving a sync baseline without the lease is no longer possible outside tests.

## CI and tooling

- `cargo test` no longer leaves `.tmp*` folders in `%TEMP%` (#43) or `ferrum-*` folders with `nul` entries (#44). Test code only, no production change.
- `cargo fmt --check` is clean again for `activity/store` (formatting only, #28).

## Internal, not enabled (Automation)

Pull requests #32 to #42 and #45 add backend and UI code for scheduled and folder-watching automations: approval, queue and status, an in-app host, scheduler, folder watcher, and the Automation list, editor and detail pages, plus the automation store at schema version 2 (see UPGRADE-NOTES.md). **None of this is available to users in 0.2.1.** The Automation UI is behind the build flag `VITE_FERRUM_AUTOMATION_UI`, which is off in release builds (the sidebar entry is greyed out), so a user cannot create an automation and the scheduler and watcher have nothing to run.

## Not included in this release

- Automation (see above), MSI installer, code signing, auto-update, Linux and macOS.
