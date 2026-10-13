import { invoke } from '@tauri-apps/api/core';

/** Raw bytes of one PDF (backend: path guard, 50 MiB cap, %PDF- magic). */
export function readPdfBytes(path: string): Promise<ArrayBuffer> {
  return invoke<ArrayBuffer>('read_pdf_bytes', { path });
}
