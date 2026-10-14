/// <reference types="svelte" />
/// <reference types="vitest" />
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { defineConfig, type Plugin } from 'vite';
import { build as esbuild } from 'esbuild';
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

function getPackageVersion(): string {
  try {
    const pkg = JSON.parse(readFileSync(resolve('package.json'), 'utf-8'));
    return typeof pkg.version === 'string' ? pkg.version : '0.0.0';
  } catch {
    return '0.0.0';
  }
}

function getGitInfo(): { commit: string; dirty: boolean } {
  try {
    const commit = execFileSync('git', ['rev-parse', '--short=7', 'HEAD'], {
      stdio: ['ignore', 'pipe', 'ignore'],
      encoding: 'utf-8',
      timeout: 3000,
    }).trim();

    const status = execFileSync('git', ['status', '--porcelain'], {
      stdio: ['ignore', 'pipe', 'ignore'],
      encoding: 'utf-8',
      timeout: 3000,
    }).trim();

    return {
      commit: commit || 'unknown',
      dirty: status.length > 0,
    };
  } catch {
    return { commit: 'unknown', dirty: false };
  }
}

const isTest = Boolean(process.env.VITEST);

const buildInfo = isTest
  ? {
      version: getPackageVersion(),
      commit: 'test000',
      dirty: false,
      date: '2026-10-14T00:00:00.000Z',
    }
  : {
      version: getPackageVersion(),
      ...getGitInfo(),
      date: new Date().toISOString(),
    };

// Tauri dev server: fixed port, strictPort so a stale server fails loudly
// instead of silently falling back to another origin the Rust shell never loads.
export default defineConfig({
  define: {
    __FERRUM_BUILD__: JSON.stringify(buildInfo),
  },
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
    include: ['src/**/*.test.ts', 'scripts/**/*.test.mjs'],
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
