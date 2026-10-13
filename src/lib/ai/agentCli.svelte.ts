// FER-490 "Ask AI agent": pertanyaan tentang lampiran dijawab oleh agent CLI lokal (Claude Code,
// Codex) yang membaca file sendiri dalam mode baca-saja yang dipaksa agent itu.
// Webview hanya mengirim id agent, pertanyaan dan path lampiran. Program, flag dan folder yang
// boleh dibaca ditentukan backend (hasil deteksi + pemeriksaan lampiran). Jawaban = teks saja:
// ditampilkan lewat MarkdownText (tanpa link/HTML) dan tidak pernah di-parse menjadi intent.
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { AskTurn } from './attachments';

export type AgentId = 'claude' | 'codex' | 'omp' | 'antigravity' | 'opencode' | 'gemini' | 'aider';
export type AgentStatus = 'supported' | 'unsupported' | 'notFound';

export interface AgentInfo {
  id: AgentId;
  name: string;
  vendor: string;
  status: AgentStatus;
  /** Path lengkap executable yang dipakai (hasil deteksi). */
  path?: string;
  version?: string;
  reason?: string;
  /** Mode baca-saja yang dipaksa Ferrum (agent yang didukung). */
  mode?: string;
  acknowledged: boolean;
  /** FER-490 E2: agent bisa membaca file lain di PC (Codex); nonaktif sampai user mengaktifkan. */
  broadRead: boolean;
  /** Bisa dipilih sebagai tujuan sekarang (didukung dan, untuk baca luas, diaktifkan). */
  enabled: boolean;
  /** FER-494: bisa dibuka interaktif di terminal (ditemukan dan lolos cek ACL). */
  terminal: boolean;
  /** FER-494: alasan "Buka di terminal" tidak tersedia. */
  terminalReason?: string;
  /** FER-494: catatan saat dibuka (mis. Codex bisa membaca file lain). */
  terminalNote?: string;
}

/** Item di folder lampiran yang tidak disalin untuk agent. */
export interface SkippedItem {
  path: string;
  reason: 'sensitive' | 'link' | 'offline' | 'unreadable';
}

export interface AgentCliView {
  agents: AgentInfo[];
  defaultAgent?: AgentId;
  detectedAtMs: number;
  testProfile: boolean;
  /** FER-494: agent untuk tombol "Buka di terminal" di composer. */
  terminalDefault?: AgentId;
}

export interface AgentAskInput {
  requestId: string;
  agent: AgentId;
  question: string;
  history: AskTurn[];
  attachments: { path: string; confirmedSensitive: boolean }[];
  signal: AbortSignal;
  /** Potongan jawaban (`text`) atau status (`status`, teks aplikasi) selama agent bekerja. */
  onEvent: (kind: 'text' | 'status', text: string) => void;
}

export interface AgentOutcome {
  agent: AgentId;
  answer: string;
  truncated: boolean;
  skipped: SkippedItem[];
}

const SKIP_REASONS = ['sensitive', 'link', 'offline', 'unreadable'];

export function parseSkipped(raw: unknown): SkippedItem[] {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter(
      (s): s is SkippedItem =>
        !!s &&
        typeof (s as SkippedItem).path === 'string' &&
        SKIP_REASONS.includes((s as SkippedItem).reason),
    )
    .map((s) => ({ path: s.path, reason: s.reason }));
}

export class AgentAskError extends Error {
  constructor(
    public readonly kind: string,
    public readonly path?: string,
    public readonly detail?: string,
  ) {
    super(kind);
    this.name = 'AgentAskError';
  }
}

/** Port yang dipakai ChatStore (disuntikkan agar store bisa diuji tanpa Tauri). */
export interface AgentAsker {
  ask(input: AgentAskInput): Promise<AgentOutcome>;
  acknowledge(agent: AgentId): Promise<void>;
}

export const AGENT_IDS: AgentId[] = [
  'claude',
  'codex',
  'omp',
  'antigravity',
  'opencode',
  'gemini',
  'aider',
];
const STATUSES: AgentStatus[] = ['supported', 'unsupported', 'notFound'];

function str(v: unknown): string | undefined {
  return typeof v === 'string' ? v : undefined;
}

export function parseView(raw: unknown): AgentCliView {
  const r = (raw ?? {}) as Record<string, unknown>;
  const agents = Array.isArray(r.agents) ? r.agents : [];
  return {
    agents: agents
      .map((a: Record<string, unknown>) => ({
        id: a.id as AgentId,
        name: str(a.name) ?? String(a.id),
        vendor: str(a.vendor) ?? '',
        status: a.status as AgentStatus,
        path: str(a.path),
        version: str(a.version),
        reason: str(a.reason),
        mode: str(a.mode),
        acknowledged: a.acknowledged === true,
        broadRead: a.broadRead === true,
        enabled: a.enabled === true,
        terminal: a.terminal === true,
        terminalReason: str(a.terminalReason),
        terminalNote: str(a.terminalNote),
      }))
      .filter((a) => AGENT_IDS.includes(a.id) && STATUSES.includes(a.status)),
    defaultAgent: AGENT_IDS.includes(r.defaultAgent as AgentId)
      ? (r.defaultAgent as AgentId)
      : undefined,
    detectedAtMs: Number(r.detectedAtMs) || 0,
    testProfile: r.testProfile === true,
    terminalDefault: AGENT_IDS.includes(r.terminalDefault as AgentId)
      ? (r.terminalDefault as AgentId)
      : undefined,
  };
}

function errorOf(error: unknown): AgentAskError {
  if (error instanceof AgentAskError) return error;
  if (error && typeof error === 'object' && 'kind' in error) {
    const e = error as { kind: unknown; path?: unknown; message?: unknown };
    if (typeof e.kind === 'string' && /^[a-z_]{1,40}$/.test(e.kind))
      return new AgentAskError(e.kind, str(e.path), str(e.message)?.slice(0, 300));
  }
  return new AgentAskError('backend_failure');
}

// ── Store pengaturan agent (Settings dan composer) ────────────────────────────────────────────

export class AgentCliStore {
  view = $state<AgentCliView | undefined>(undefined);
  loading = $state(false);
  error = $state<string | undefined>(undefined);
  private pending: Promise<void> | null = null;

  /** Agent yang didukung DAN aktif (pilihan tujuan di composer). Baca luas = nonaktif default. */
  get supported(): AgentInfo[] {
    return this.view?.agents.filter((a) => a.status === 'supported' && a.enabled) ?? [];
  }

  get defaultAgent(): AgentInfo | undefined {
    const id = this.view?.defaultAgent;
    return this.supported.find((a) => a.id === id) ?? this.supported[0];
  }

  /** FER-494: agent yang bisa dibuka di terminal (semua yang ditemukan dan lolos ACL). */
  get terminalAgents(): AgentInfo[] {
    return this.view?.agents.filter((a) => a.terminal) ?? [];
  }

  /** FER-494: agent tombol "Buka di terminal" (pilihan Settings, atau yang pertama tersedia). */
  get terminalDefault(): AgentInfo | undefined {
    const id = this.view?.terminalDefault;
    return this.terminalAgents.find((a) => a.id === id) ?? this.terminalAgents[0];
  }

  agent(id: AgentId): AgentInfo | undefined {
    return this.view?.agents.find((a) => a.id === id);
  }

  /** Muat sekali (backend mendeteksi sekali bila belum pernah). */
  load(): Promise<void> {
    if (this.view) return Promise.resolve();
    this.pending ??= this.call('agent_cli_status').finally(() => (this.pending = null));
    return this.pending;
  }

  redetect(): Promise<void> {
    return this.call('agent_cli_detect');
  }

  setDefault(id: AgentId): Promise<void> {
    return this.call('agent_cli_set_default', { agent: id });
  }

  acknowledge(id: AgentId): Promise<void> {
    return this.call('agent_cli_acknowledge', { agent: id });
  }

  /** FER-494: agent default untuk "Buka di terminal". */
  setTerminalDefault(id: AgentId): Promise<void> {
    return this.call('agent_cli_set_terminal_default', { agent: id });
  }

  /** FER-490 E2: opt-in eksplisit untuk agent baca luas (disimpan di backend). */
  setEnabled(id: AgentId, enabled: boolean): Promise<void> {
    return this.call('agent_cli_set_enabled', { agent: id, enabled });
  }

  private async call(cmd: string, args?: Record<string, unknown>): Promise<void> {
    this.loading = true;
    this.error = undefined;
    try {
      this.view = parseView(await invoke<unknown>(cmd, args));
    } catch {
      this.error = 'Daftar agent CLI tidak dapat dimuat.';
    } finally {
      this.loading = false;
    }
  }
}

export const agentCli = new AgentCliStore();

/** Test hook. */
export function resetAgentCliForTests(): void {
  agentCli.view = undefined;
  agentCli.loading = false;
  agentCli.error = undefined;
}

// ── Port produksi ─────────────────────────────────────────────────────────────────────────────

export function tauriAgentAsker(store: AgentCliStore = agentCli): AgentAsker {
  return {
    async ask(input) {
      const unlisten = await listen<{ requestId?: string; kind?: string; text?: string }>(
        'agent_cli://event',
        (e) => {
          const p = e.payload;
          if (!p || p.requestId !== input.requestId || typeof p.text !== 'string') return;
          if (p.kind === 'text' || p.kind === 'status') input.onEvent(p.kind, p.text);
        },
      );
      const onAbort = () => {
        void invoke('ai_cancel_plan', { requestId: input.requestId }).catch(() => undefined);
      };
      input.signal.addEventListener('abort', onAbort, { once: true });
      try {
        const raw = await invoke<Record<string, unknown>>('agent_ask', {
          request: {
            requestId: input.requestId,
            agent: input.agent,
            question: input.question,
            history: input.history,
            attachments: input.attachments,
          },
        });
        if (input.signal.aborted) throw new AgentAskError('cancelled');
        if (!raw || typeof raw.answer !== 'string') throw new AgentAskError('malformed');
        return {
          agent: input.agent,
          answer: raw.answer,
          truncated: raw.truncated === true,
          skipped: parseSkipped(raw.skipped),
        };
      } catch (error) {
        const e = errorOf(error);
        throw input.signal.aborted ? new AgentAskError('cancelled') : e;
      } finally {
        input.signal.removeEventListener('abort', onAbort);
        unlisten();
      }
    },
    acknowledge: (agent) => store.acknowledge(agent),
  };
}

/** Teks galat untuk chat (teks aplikasi; `detail` dari agent ditampilkan sebagai teks biasa). */
export function agentErrorText(e: AgentAskError, agentName: string): string {
  switch (e.kind) {
    case 'cancelled':
      return 'Dibatalkan.';
    case 'timeout':
      return `${agentName} tidak selesai dalam 5 menit; proses dihentikan.`;
    case 'disclosure_required':
      return `Baca dan setujui pemberitahuan untuk ${agentName} dulu.`;
    case 'agent_unavailable':
      return `${agentName} tidak tersedia. Buka Settings → AI → Agent CLI lalu Deteksi ulang.`;
    case 'agent_start_failed':
      return `${agentName} tidak dapat dijalankan.`;
    case 'attachment_unavailable':
      return 'Lampiran harus file atau folder biasa di disk (bukan isi arsip) dan masih ada.';
    case 'attachment_link':
      return 'Tautan/junction tidak dikirim ke agent (agent akan mengikutinya).';
    case 'attachment_offline':
      return 'File cloud yang belum diunduh tidak dikirim ke agent (membacanya akan mengunduh file).';
    case 'folder_too_broad':
      return 'Folder terlalu luas untuk dilampirkan ke agent (akar drive, folder profil pengguna, atau Windows).';
    case 'snapshot_limit':
      return `Lampiran melebihi batas salinan untuk agent${e.detail ? ` (${e.detail})` : ''}. Batas: 100 MB, 2000 file dan folder, kedalaman folder 8.`;
    case 'agent_untrusted':
      return `${agentName} tidak dijalankan: ${e.detail ?? 'lokasinya bisa ditulis akun lain'}.`;
    case 'sensitive_unconfirmed':
      return 'Lampiran terlihat sensitif dan belum dicentang. Tidak ada yang dikirim.';
    case 'empty_answer':
      return `${agentName} tidak memberi jawaban.`;
    case 'agent_failed':
      return e.detail ? `${agentName} gagal: ${e.detail}` : `${agentName} gagal.`;
    default:
      return `${agentName} gagal (${e.kind}).`;
  }
}

const SKIP_TEXT: Record<SkippedItem['reason'], string> = {
  sensitive: 'terlihat sensitif',
  link: 'tautan/junction',
  offline: 'file cloud belum diunduh',
  unreadable: 'tidak dapat dibaca',
};

/** "nama (alasan)" untuk item yang tidak disalin ke agent. */
export function skippedText(s: SkippedItem): string {
  const name = s.path.split(/[\\/]/).pop() ?? s.path;
  return `${name} (${SKIP_TEXT[s.reason]})`;
}

/** Lampiran yang tidak bisa dibaca utuh oleh AI Ferrum: tawarkan agent. */
export function needsAgent(
  info: {
    kind: string;
    support: string;
    size: number;
    inArchive: boolean;
  },
  maxFileBytes: number,
): boolean {
  if (info.inArchive) return false;
  return (
    info.kind === 'pdf' ||
    info.kind === 'document' ||
    (info.support === 'metadata' && info.kind !== 'link') ||
    (info.support === 'content' && info.size > maxFileBytes)
  );
}
