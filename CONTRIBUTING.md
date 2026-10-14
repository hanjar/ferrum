# Contributing to Ferrum

Thank you for your interest in contributing to Ferrum!

## Code of Conduct & Principles

- **Security & Safety First**: File manager operations touch user data directly. Defensive file handling (preventing path traversal, validating boundaries, guarding against symlink loops and race conditions) is mandatory.
- **Windows Focus**: Ferrum is currently developed and tested primarily for Windows 10 and 11.

## Prerequisites

- **OS**: Windows 10 or 11 (x64)
- **Node.js**: v20 or newer
- **Rust**: 1.98.1 (pinned by `rust-toolchain.toml`; `rustup` installs it automatically) with MSVC (`x86_64-pc-windows-msvc`)
- **Visual Studio Build Tools**: C++ build tools installed for MSVC and Windows SDK

## Getting Started

1. Clone your fork:

   ```powershell
   git clone https://github.com/<your-username>/ferrum.git
   cd ferrum
   ```

2. Install frontend dependencies:

   ```powershell
   npm ci
   ```

3. Run in development mode:
   ```powershell
   npm run dev
   ```
   Or launch the full desktop application via Tauri:
   ```powershell
   npx tauri dev
   ```

## Verification & Quality Gates

All pull requests must pass the complete quality gate before merging. Run the following checks locally:

### Frontend Checks

```powershell
# Format checking
npx prettier --check .

# ESLint
npm run lint

# Svelte type check
npx svelte-check --threshold error

# Vitest unit & integration tests
npx vitest run

# Production frontend build
npm run build
```

### Rust Backend Checks

```powershell
cd src-tauri

# Code formatting
cargo fmt -- --check

# Clippy linter
cargo clippy --workspace --all-targets -- -D warnings

# Workspace tests
cargo test --workspace

# Indexer crate checks
cd crates\indexer
cargo clippy --all-targets -- -D warnings
cargo test
```

## Third-Party Notices

`THIRD-PARTY-NOTICES.txt` is generated and shipped with the installer. If you add or update a dependency (`package-lock.json` or `src-tauri/Cargo.lock` changes), regenerate it and commit the result:

```powershell
npm ci
node scripts/generate-third-party-notices.mjs
```

CI runs `node scripts/generate-third-party-notices.mjs --check` and fails when the file is stale. The generator also fails for a dependency without a license file or with a license outside its allowlist (`scripts/lib/third-party-notices.mjs`); do not add GPL, AGPL, LGPL, SSPL, BUSL, or unlicensed code without discussing it first.

## Commit Guidelines

- Use clear and descriptive commit messages following the Conventional Commits style where appropriate:
  - `feat: ...` for new features
  - `fix: ...` for bug fixes
  - `docs: ...` for documentation changes
  - `refactor: ...` for code refactoring
  - `test: ...` for test additions or updates
- Do not commit third-party binaries, build outputs (`dist/`, `target/`, `node_modules/`), or personal configurations.

## Licensing of Contributions

By contributing to Ferrum, you agree that your contributions will be dual-licensed under both the **MIT License** and the **Apache License, Version 2.0**, in accordance with the project's licensing terms.
