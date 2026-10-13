/// <reference types="svelte" />
/// <reference types="vitest" />
import { defineConfig, type Plugin } from 'vite';
import { build as esbuild } from 'esbuild';
import { resolve } from 'node:path';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { svelteTesting } from '@testing-library/svelte/vite';

// FER-453: the sandboxed PDF viewer is one classic IIFE script (public/pdf-viewer.js),
// bundled from src/pdf-viewer/main.ts with pdf.js and its worker in-thread. It is rebuilt
// on every Vite start (dev server, Playwright's `npx vite`, and build), so it can never be
// older than its sources. A failed bundle stops the start with the error. The file is
// git-ignored.
function pdfViewerBundle(): Plugin {
  return {
    name: 'ferrum-pdf-viewer-bundle',
    async buildStart() {
      try {
        await esbuild({
          entryPoints: [resolve('src/pdf-viewer/main.ts')],
          bundle: true,
          format: 'iife',
          target: 'es2022',
          minify: true,
          legalComments: 'none',
          logLevel: 'warning',
          outfile: resolve('public/pdf-viewer.js'),
        });
      } catch (e) {
        throw new Error(`PDF viewer bundle failed (src/pdf-viewer/main.ts): ${String(e)}`);
      }
    },
  };
}

// Tauri dev server: fixed port, strictPort so a stale server fails loudly
// instead of silently falling back to another origin the Rust shell never loads.
export default defineConfig({
  plugins: [svelte(), svelteTesting(), pdfViewerBundle()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    target: 'es2022',
    minify: 'esbuild',
    sourcemap: false,
    // FER-500: the app CSP only allows fonts from 'self' (font-src 'self'), so inlined
    // data: woff2 subsets are blocked at runtime. false = always emit font files instead.
    assetsInlineLimit: (filePath) =>
      /\.(woff2?|ttf|otf|eot)$/i.test(filePath) ? false : undefined,
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['src/test-setup.ts'],
    include: ['src/**/*.test.ts'],
    coverage: {
      provider: 'v8',
      // Coverage measures logic modules; components gain tests from task 05
      // onward (interaction/virtualization suites) and join this list then.
      include: [
        'src/lib/ipc/**',
        'src/lib/stores/**',
        'src/lib/format.ts',
        'src/lib/kind.ts',
        'src/lib/version.ts',
      ],
      reporter: ['text', 'lcov'],
      thresholds: { lines: 80, functions: 80, branches: 75, statements: 80 },
    },
  },
});
