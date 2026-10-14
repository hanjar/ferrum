// FER-494 "Buka di terminal": buka agent CLI secara INTERAKTIF di terminal yang terlihat, di folder
// yang dipilih. Setelah itu user yang memimpin, dengan izin bawaan agent itu.
//
// Webview hanya mengirim id agent, path folder/file dan preferensi terminal. Program, argumen dan
// pemeriksaan ACL ditentukan backend (`agent_cli/terminal.rs`). TIDAK ADA teks dari user di
// command line: pertanyaan dari composer disalin ke clipboard lalu ditempel user sendiri.
import { invoke } from '@tauri-apps/api/core';
import { agentCli, type AgentCliStore, type AgentId } from './agentCli.svelte';
import { copyText } from '../clipboard';
import { toast } from '../stores/toastStore.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { contextMenu, type ContextMenuItem } from '../stores/contextMenuStore.svelte';
import { ui } from '../uiState.svelte';
import { isInTauri } from '../ipc/tauri';

/** Nama agent untuk label registry (dipakai sebelum daftar dari backend dimuat). */
export const AGENT_LABELS: Record<AgentId, string> = {
  claude: 'Claude Code',
  codex: 'Codex',
  omp: 'omp',
  antigravity: 'Antigravity',
  opencode: 'opencode',
  gemini: 'Gemini CLI',
  aider: 'Aider',
};

export interface TerminalOpenRequest {
  agent: AgentId;
  path: string;
  program: string;
  // FER-516: no "confirmed" flag. For an untrusted folder the backend raises its own native
  // confirmation; nothing the webview sends can stand in for it.
}

export interface TerminalOpened {
  agent: AgentId;
  via?: 'windowsTerminal' | 'console';
  argv: string[];
  cwd: string;
  dryRun: boolean;
  note?: string;
  /** Konfigurasi proyek, folder UNC, atau pemilik folder lain (satu baris per temuan). */
  warnings: string[];
}

/** Port ke backend (disuntikkan di tes). Perhatikan: tidak ada field untuk teks pertanyaan. */
export interface TerminalPort {
  open(request: TerminalOpenRequest): Promise<TerminalOpened>;
}

const tauriPort: TerminalPort = {
  open: (request) => invoke<TerminalOpened>('agent_terminal_open', { request }),
};
let port: TerminalPort = tauriPort;

/** Test hook. */
export function setTerminalPortForTests(p: TerminalPort | undefined): void {
  port = p ?? tauriPort;
}

const autoLoaded = new WeakSet<AgentCliStore>();

/**
 * Muat daftar agent sekali (desktop app saja). Aman dipanggil dari `available()` yang dibaca di
 * dalam `$derived`: pemuatan (yang mengubah state) ditunda ke microtask.
 */
export function ensureAgentsLoaded(store: AgentCliStore = agentCli): void {
  // One automatic attempt per store: a failed load must not re-trigger itself through the
  // derived values that read it (Settings/"Deteksi ulang" can still load again).
  if (store.view || store.loading || autoLoaded.has(store) || !isInTauri()) return;
  autoLoaded.add(store);
  queueMicrotask(() => void store.load());
}

/** Agent ini bisa dibuka di terminal sekarang. */
export function terminalReady(id: AgentId, store: AgentCliStore = agentCli): boolean {
  ensureAgentsLoaded(store);
  return store.agent(id)?.terminal === true;
}

function kindOf(error: unknown): { kind: string; detail?: string } {
  if (error && typeof error === 'object' && 'kind' in error) {
    const e = error as { kind: unknown; message?: unknown };
    if (typeof e.kind === 'string' && /^[a-z_]{1,40}$/.test(e.kind))
      return {
        kind: e.kind,
        detail: typeof e.message === 'string' ? e.message.slice(0, 300) : undefined,
      };
  }
  return { kind: 'backend_failure' };
}

export function terminalErrorText(kind: string, name: string, detail?: string): string {
  switch (kind) {
    case 'agent_unavailable':
      return detail
        ? `${name} tidak bisa dibuka di terminal: ${detail}.`
        : `${name} tidak tersedia. Buka Settings → AI → Agent CLI lalu Deteksi ulang.`;
    case 'agent_untrusted':
      return `${name} tidak dijalankan: ${detail ?? 'lokasinya bisa ditulis akun lain'}.`;
    case 'folder_invalid':
      return 'Folder ini tidak bisa dipakai untuk agent (path perangkat, \\\\?\\, karakter tidak sah, atau segmen . / ..).';
    case 'folder_missing':
      return 'Folder tidak ditemukan.';
    case 'busy':
      return 'Konfirmasi lain masih terbuka. Selesaikan dulu, lalu coba lagi.';
    case 'terminal_start_failed':
      return `Terminal untuk ${name} tidak dapat dibuka.`;
    default:
      return `${name} tidak dapat dibuka (${kind}).`;
  }
}

/**
 * Buka `agent` di terminal pada `path` (folder; untuk file dipakai folder induknya). Pertanyaan
 * yang sudah diketik HANYA disalin ke clipboard. `extraNote` ditambahkan ke toast.
 */
export async function openAgentTerminal(
  agent: AgentId,
  path: string,
  opts: {
    question?: string;
    extraNote?: string;
    store?: AgentCliStore;
  } = {},
): Promise<TerminalOpened | undefined> {
  const store = opts.store ?? agentCli;
  const name = store.agent(agent)?.name ?? AGENT_LABELS[agent];
  const question = opts.question?.trim() ?? '';
  // Disalin sebelum terminal mengambil fokus (clipboard webview butuh dokumen yang fokus).
  const copied = question ? await copyText(question) : false;
  let opened: TerminalOpened;
  try {
    opened = await port.open({
      agent,
      path,
      program: ui.terminal,
    });
  } catch (error) {
    const { kind, detail } = kindOf(error);
    // The user declined the native confirmation: a neutral note, not an error.
    if (kind === 'cancelled') {
      toast.show(`${name} tidak dibuka.`, 4000);
      return undefined;
    }
    if (kind === 'busy') {
      toast.show(terminalErrorText(kind, name), 5000);
      return undefined;
    }
    dialogs.open({
      kind: 'error',
      title: `Buka ${name}`,
      message: terminalErrorText(kind, name, detail),
    });
    return undefined;
  }
  const parts = [
    opened.dryRun ? `Profil uji: ${name} tidak dijalankan.` : `${name} dibuka di terminal.`,
  ];
  if (question) {
    parts.push(
      copied
        ? `Pertanyaan disalin — tempel di ${name} (Ctrl+V).`
        : 'Pertanyaan tidak dapat disalin ke clipboard.',
    );
  }
  if (opened.note) parts.push(opened.note);
  // Findings of the folder check (the user confirmed in the native dialog, or the agent asks
  // about folder trust itself): shown as a note.
  if (opened.warnings?.length) parts.push(`Catatan: ${opened.warnings.join(' ')}`);
  if (opts.extraNote) parts.push(opts.extraNote);
  toast.show(parts.join(' '), 7000);
  return opened;
}

/** Submenu "Buka di agent ▸": dibuka di posisi menu yang baru ditutup. */
export async function openAgentSubmenu(path: string, store: AgentCliStore = agentCli) {
  const { x, y, owner } = contextMenu;
  // Synchronously when the list is loaded: the menu reopens in the same task, before the pane
  // restores focus to its row (which could scroll and close the menu again).
  if (!store.view) await store.load();
  const list = store.terminalAgents;
  if (!list.length) {
    toast.show(
      'Tidak ada agent yang bisa dibuka di terminal. Lihat Settings → AI → Agent CLI.',
      5000,
    );
    return;
  }
  const items: ContextMenuItem[] = [
    { label: 'Buka di agent:', disabled: true, run: () => undefined },
    ...list.map((a) => ({
      label: a.name,
      run: () => void openAgentTerminal(a.id, path, { store }),
    })),
  ];
  // Posisi klien semula; open() menjepitnya lagi ke viewport.
  contextMenu.open(x, y, items, owner);
}

/** Item menu konteks (desktop app saja; di browser dev tidak ada agent). */
export function agentSubmenuItem(path: string): ContextMenuItem | undefined {
  if (!isInTauri()) return undefined;
  return { label: 'Buka di agent ▸', run: () => void openAgentSubmenu(path) };
}

/** Folder lampiran pertama (untuk file: folder induknya) dan apakah ada folder lain. */
export function firstAttachmentFolder(attachments: { path: string; isDir: boolean }[]): {
  folder?: string;
  others: boolean;
} {
  const parent = (p: string) => {
    const i = p.replace(/[\\/]+$/, '').search(/[\\/][^\\/]*$/);
    if (i <= 0) return p;
    const head = p.slice(0, i);
    return /^[A-Za-z]:$/.test(head) ? `${head}\\` : head;
  };
  const folders = attachments.map((a) => (a.isDir ? a.path : parent(a.path)));
  const norm = (p: string) => p.replace(/[\\/]+$/, '').toLowerCase();
  const first = folders[0];
  return {
    folder: first,
    others: first !== undefined && folders.some((f) => norm(f) !== norm(first)),
  };
}
