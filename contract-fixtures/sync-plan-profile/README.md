# Profile Sync Plan Contract Fixtures

This directory contains fixtures specifically for automation profile mode synchronization plans.

## Purpose

Unlike ad-hoc sync (which follows TypeScript's older contract where whole folders could be recursively copied or deleted), profile mode strictly enforces granular non-recursive directory actions:
- `createDirRight` / `createDirLeft`: directory creation (`mkdir`) only, without copying contents (contents are handled by their own individual item actions).
- `removeEmptyDirRight` / `removeEmptyDirLeft`: empty directory removal (`remove_dir`) only. If a directory is non-empty at execution time (e.g. because it contains excluded files), the deletion is skipped safely without error.

In profile mode, folder entries (`is_dir: true`) NEVER receive recursive actions (`copyToRight`, `copyToLeft`, `deleteRight`, `deleteLeft`), ensuring whole folder operations can never bypass include/exclude profile filters.

## Test Harness

These fixtures are tested exclusively by Rust test `test_profile_sync_plan_fixtures` in `src-tauri/src/sync_engine/plan.rs`. They are partitioned from `contract-fixtures/sync-plan/` so as not to break TypeScript parity tests, which do not implement profile non-recursive folder actions.
