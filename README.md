# Ferrum

Ferrum is a modern, fast, keyboard-centric dual-pane file manager for Windows inspired by classic Total Commander workflows, built using **Tauri 2**, **Rust**, and **Svelte 5**.

> [!NOTE]
> **Status: Pre-release (v0.2.0)**  
> Ferrum is actively being developed. Installers and binaries are currently unsigned, meaning Windows SmartScreen will display an unrecognized app warning during installation or first launch.

---

## Highlights & Features

Features in Ferrum are designed with safety, reliability, and keyboard productivity in mind. Facts and capabilities are aligned with the project's verification test suite:

- **Dual-Pane Interface**:
  - Independent left and right panes with tab strips, breadcrumbs, history navigation, and customizable split dividers.
  - Session restoration across restarts (tabs, active locations, and pane layout).
  - Directory Tree panel, Places sidebar, and Directory Hotlist (`Ctrl+D`).
  - Branch View (`Ctrl+B`) showing all recursive files flat in a single pane.

- **Fast & Safe File Operations**:
  - Non-blocking asynchronous queue with Background Transfer Manager (`Alt+B`), progress indicators, pause/cancel support, and batch conflict resolution (Skip, Overwrite, Auto-Rename).
  - Default delete moves files safely to the Windows Recycle Bin (`F8`), with permanent deletion available via confirmation (`Shift+F8`).
  - Multi-Rename Tool (`Ctrl+M`) with search/replace, numbering, and real-time rename preview.
  - Attribute viewing/editing, Calculate Occupied Space (`Ctrl+L`), and file comments sidecar (`.ferrum/comments.json`).

- **Archive Support**:
  - **ZIP & 7z**: Browse archives as virtual folders (`Enter`), extract all or selection (`Alt+F9`), create archives (`Alt+F5`), and verify integrity (`Alt+Shift+F9`) with strict path sanitation (guarding against directory traversal/zip-slip attacks).
  - **RAR / RAR5 Support**:
    - **Reading & Extracting**: Supported read-only through embedded UnRAR (supports virtual browsing and extraction).
    - **Creating RAR Archives**: Supported optionally through a user-installed, licensed WinRAR (`Rar.exe`) executable configured via Settings. Ferrum does not distribute WinRAR binaries.

- **Search, Compare & Sync**:
  - In-folder search (`Ctrl+F`) and advanced search (`Alt+F7`) with capped execution and direct results feed to panes.
  - Compare Directories (`Shift+F2`) and Compare by Content (binary and side-by-side text diffing).
  - Synchronize Directories with mandatory baseline comparison and dry-run confirmation before any modification.

- **Inspection & Utilities**:
  - **Quick View (`Ctrl+Q`)**: Instant inspection of text files, JSON, Markdown, images, audio/video metadata, and sandboxed PDF viewing (powered by pdf.js).
  - **Disk Usage Workspace (`Alt+Shift+D`)**: Fast volume and folder space breakdown.
  - **File Tools**: Generate and verify checksums (MD5, SHA-1, SHA-256), split and combine files, and encode/decode (Base64, UUE, XXE).

- **Personalization & Keyboard Focus**:
  - Six color palettes (Violet, Emerald, etc.) with dark, light, and system theme auto-switching.
  - Configurable interface font scaling (S, M, L, XL), row density, and icon styles.
  - Command Palette (`Ctrl+K`) for fast action execution, plus custom keybar.
  - Privacy-conscious Activity tracking: recent and frequent folders with private session mode and full data clearing.

---

## System Requirements & Prerequisites

Ferrum is built for Windows (Windows 10 and 11, 64-bit).

To build Ferrum from source:

- **Operating System**: Windows 10/11 x64
- **Node.js**: v20 or newer
- **Rust**: 1.80+ with the MSVC toolchain (`x86_64-pc-windows-msvc`)
- **C++ Build Tools**: Visual Studio 2022 Build Tools (with C++ Desktop development and Windows SDK)

---

## Building from Source

1. **Clone the repository**:

   ```powershell
   git clone https://github.com/hanjar/ferrum.git
   cd ferrum
   ```

2. **Install frontend dependencies**:

   ```powershell
   npm ci
   ```

3. **Run in development mode**:

   ```powershell
   npm run dev
   ```

   Or run the desktop application with Tauri live reloading:

   ```powershell
   npx tauri dev
   ```

4. **Build the production executable & installer**:
   ```powershell
   npm run build
   npx tauri build --bundles nsis
   ```
   The installer executable will be generated under `src-tauri/target/release/bundle/nsis/`.

---

## Testing & Quality Gates

Run the automated test suites to verify functionality:

```powershell
# Frontend lint, formatting, and unit tests
npx prettier --check .
npm run lint
npx svelte-check --threshold error
npx vitest run

# Rust backend formatting, clippy, and unit/integration tests
cd src-tauri
cargo fmt -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Indexer service crate
cd crates\indexer
cargo clippy --all-targets -- -D warnings
cargo test
```

---

## License

Ferrum is dual-licensed under either:

- **MIT License** ([LICENSE-MIT](LICENSE-MIT))
- **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

Third-party libraries, fonts, and tools incorporated into or used by Ferrum are subject to their respective licenses. See [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) for full attribution and licensing details.
