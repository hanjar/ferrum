/// <reference types="svelte" />
/// <reference types="vite/client" />

declare module '*.css';

declare module '*.svelte' {
  import type { Component } from 'svelte';
  const component: Component;
  export default component;
}

// FER-453: pdf.js worker module, used in-thread by the sandboxed viewer (see pdfOpen.ts).
declare module 'pdfjs-dist/build/pdf.worker.mjs' {
  export const WorkerMessageHandler: unknown;
}
declare module 'pdfjs-dist/legacy/build/pdf.worker.mjs' {
  export const WorkerMessageHandler: unknown;
}
