// FER-465: galat provider -> pesan Indonesia yang jelas + saran + tombol aksi. Backend hanya
// mengirim kode aman (ProviderErrorKind); pemetaan pesan ada di sini. Tidak pernah memuat key,
// isi respons mentah, atau stack trace; endpoint hanya host:port.
import { ContractError } from './contracts';
import { RemoteProviderError, type RemoteProviderErrorCode } from './remote-provider';

export type ProviderUiCode =
  | 'unreachable'
  | 'timeout'
  | 'auth'
  | 'credential'
  | 'model_not_found'
  | 'bad_request'
  | 'rate_limited'
  | 'quota_exceeded'
  | 'server_error'
  | 'invalid_response'
  | 'not_supported'
  | 'invalid_config'
  | 'unknown';

export type ProviderAction = 'retry' | 'settings' | 'test';

export interface ProviderErrorView {
  code: ProviderUiCode;
  message: string;
  actions: readonly ProviderAction[];
}

/** Capability yang BISA dilakukan sekarang (id -> kalimat singkat). Id lain diabaikan. */
const CAPABILITY_TEXT: Record<string, string> = {
  'archive.create': 'membuat arsip ZIP dari file/folder di panel',
  'directories.sync': 'sinkronisasi dua folder (panel kiri dan kanan)',
  'activity.query': 'menanyakan riwayat file/folder yang pernah dibuka',
  'diskUsage.query': 'menanyakan isi hasil Disk Usage yang sedang tampil (file/folder terbesar)',
};

/** `host:port` saja dari endpoint (tanpa path, userinfo, atau query); undefined bila tidak valid. */
export function endpointHostPort(endpoint: string | undefined): string | undefined {
  if (!endpoint) return undefined;
  try {
    const url = new URL(endpoint);
    return url.host || undefined;
  } catch {
    return undefined;
  }
}

/** Kode backend/lokal -> kode UI. Kode tak dikenal jatuh ke 'unknown'. */
export function uiCodeOf(code: RemoteProviderErrorCode | 'unknown'): ProviderUiCode {
  switch (code) {
    case 'unreachable':
    case 'network':
      return 'unreachable';
    case 'timeout':
      return 'timeout';
    case 'auth':
      return 'auth';
    case 'credential':
      return 'credential';
    case 'model_unavailable':
      return 'model_not_found';
    case 'bad_request':
      return 'bad_request';
    case 'rate_limited':
      return 'rate_limited';
    case 'quota_exceeded':
      return 'quota_exceeded';
    case 'server_error':
      return 'server_error';
    case 'malformed':
      return 'invalid_response';
    case 'refused':
      return 'not_supported';
    case 'invalid_config':
    case 'unsafe_endpoint':
      return 'invalid_config';
    default:
      return 'unknown';
  }
}

/** Galat yang ditangkap executor -> kode UI. */
export function classifyProviderFailure(error: unknown): ProviderUiCode {
  if (error instanceof RemoteProviderError) return uiCodeOf(error.code);
  if (error instanceof ContractError) return 'invalid_response';
  return 'unknown';
}

export interface DescribeOptions {
  /** host:port dari `endpointHostPort`. */
  hostPort?: string;
  /** Id capability yang ditawarkan sekarang (untuk `not_supported`). */
  capabilities?: readonly string[];
}

const SUFFIX = 'Tidak ada yang dijalankan.';

export function describeProviderError(
  code: ProviderUiCode,
  options: DescribeOptions = {},
): ProviderErrorView {
  const at = options.hostPort ? ` (${options.hostPort})` : '';
  switch (code) {
    case 'unreachable':
      return {
        code,
        message: `Server AI tidak bisa dihubungi${at}. Pastikan layanan AI (mis. server lokal) sedang berjalan, lalu coba lagi. ${SUFFIX}`,
        actions: ['retry', 'settings', 'test'],
      };
    case 'timeout':
      return {
        code,
        message: `Server AI${at} terlalu lama menjawab atau macet. Pastikan layanan AI berjalan normal, lalu coba lagi. ${SUFFIX}`,
        actions: ['retry', 'settings', 'test'],
      };
    case 'auth':
      return {
        code,
        message: `Server AI${at} menolak key (salah atau kedaluwarsa). Buka Settings ▸ AI untuk mengganti key. ${SUFFIX}`,
        actions: ['settings', 'test'],
      };
    case 'credential':
      return {
        code,
        message: `Key AI belum tersimpan atau tidak bisa dibaca. Buka Settings ▸ AI untuk mengisinya. ${SUFFIX}`,
        actions: ['settings'],
      };
    case 'model_not_found':
      return {
        code,
        message: `Server AI${at} tidak mengenal model yang dipilih. Cek nama model di Settings ▸ AI. ${SUFFIX}`,
        actions: ['settings', 'test'],
      };
    case 'bad_request':
      return {
        code,
        message: `Server AI${at} menolak permintaan. Cek model dan pengaturan di Settings ▸ AI. ${SUFFIX}`,
        actions: ['settings', 'test'],
      };
    case 'rate_limited':
      return {
        code,
        message: `Server AI${at} sedang membatasi permintaan. Tunggu sebentar, lalu coba lagi. ${SUFFIX}`,
        actions: ['retry', 'test'],
      };
    case 'quota_exceeded':
      return {
        code,
        message: `Kuota atau saldo layanan AI${at} habis. Periksa akun layanan, atau ganti provider di Settings ▸ AI. ${SUFFIX}`,
        actions: ['settings', 'test'],
      };
    case 'server_error':
      return {
        code,
        message: `Server AI${at} sedang bermasalah. Coba lagi sebentar lagi. ${SUFFIX}`,
        actions: ['retry', 'test'],
      };
    case 'invalid_response':
      return {
        code,
        message: `Server AI menjawab, tetapi jawabannya tidak bisa dipahami atau tidak sesuai kontrak. Coba lagi atau ubah kalimat permintaan; bila berulang, coba model lain di Settings ▸ AI. ${SUFFIX}`,
        actions: ['retry', 'settings', 'test'],
      };
    case 'not_supported': {
      const can = (options.capabilities ?? [])
        .map((id) => CAPABILITY_TEXT[id])
        .filter((text): text is string => text !== undefined);
      const list = can.length > 0 ? ` Saat ini AI bisa: ${can.join('; ')}.` : '';
      return {
        code,
        message: `Model menolak atau permintaan ini di luar kemampuan AI.${list} ${SUFFIX}`,
        actions: [],
      };
    }
    case 'invalid_config':
      return {
        code,
        message: `Pengaturan AI belum lengkap atau tidak valid. Periksa endpoint dan model di Settings ▸ AI. ${SUFFIX}`,
        actions: ['settings'],
      };
    default:
      return {
        code: 'unknown',
        message: `Provider AI tidak dapat membuat rencana. Coba lagi, atau periksa Settings ▸ AI. ${SUFFIX}`,
        actions: ['retry', 'settings', 'test'],
      };
  }
}

/** Satu baris untuk hasil Test connection / indikator header. */
export function describeConnection(kind: RemoteProviderErrorCode | 'unknown', hostPort?: string) {
  return describeProviderError(uiCodeOf(kind), { hostPort }).message.replace(` ${SUFFIX}`, '');
}
