// Penyimpanan setup provider AI yang TIDAK rahasia (FER-430). Di aplikasi desktop setup hidup di
// backend (`ai-setup.json` di app config dir) sehingga sama untuk build dev dan build exe;
// localStorage terikat origin dan hanya dipakai di luar Tauri (browser dev, tes) serta sebagai
// sumber migrasi satu kali. Key tidak pernah lewat modul ini. Modul daun: tidak mengimpor aiRuntime
// atau providerSettings, supaya aiRuntime dapat membaca cache secara sinkron tanpa siklus impor.
import { invoke } from '@tauri-apps/api/core';
import { isInTauri } from '../ipc/tauri';
import { validateProviderSetup, type ProviderSetup } from './provider-setup';
import type { BackendErrorKind } from './remote-provider';

/** Kunci localStorage lama (sebelum FER-430). Tetap dibaca di luar Tauri dan untuk migrasi. */
export const SETUP_STORAGE_KEY = 'ferrum.ai.setup';

export type StoreErrorKind = BackendErrorKind | 'storage_unavailable';

// Salinan setup backend untuk pembacaan sinkron (aiRuntime). Diisi oleh `loadBackendSetup`/save.
let cache: ProviderSetup | undefined;
let loaded: Promise<ProviderSetup | undefined> | undefined;

function readLocal(): ProviderSetup | undefined {
  try {
    const text = globalThis.localStorage?.getItem(SETUP_STORAGE_KEY);
    return text ? validateProviderSetup(JSON.parse(text)) : undefined;
  } catch {
    return undefined;
  }
}

function dropLocal(): void {
  try {
    globalThis.localStorage?.removeItem(SETUP_STORAGE_KEY);
  } catch {
    /* storage tidak tersedia */
  }
}

function backendKind(error: unknown): StoreErrorKind {
  const kind = error && typeof error === 'object' ? (error as { kind?: unknown }).kind : undefined;
  return kind === 'unsafe_endpoint' || kind === 'invalid_config' ? kind : 'storage_unavailable';
}

/** Setup valid saat ini (sinkron). Di Tauri dari cache backend; selainnya dari localStorage. */
export function currentSetup(): ProviderSetup | undefined {
  return isInTauri() ? cache : readLocal();
}

/**
 * Muat setup dari backend sekali (single-flight) dan migrasikan localStorage lama bila backend
 * belum punya setup: salin ke backend, lalu hapus salinan lama HANYA setelah backend menerimanya.
 * Di luar Tauri tidak ada backend; hasilnya localStorage. Tidak pernah melempar.
 */
export function loadSetup(): Promise<ProviderSetup | undefined> {
  if (!isInTauri()) return Promise.resolve(readLocal());
  loaded ??= (async () => {
    try {
      const raw = await invoke<unknown>('ai_setup_load');
      if (raw) {
        cache = validateProviderSetup(raw);
        dropLocal(); // backend sudah punya setup; salinan lama tidak lagi dipakai
        return cache;
      }
    } catch {
      // backend tidak bisa dibaca atau isinya tidak valid: coba migrasi dari salinan lama
    }
    const legacy = readLocal();
    if (legacy) {
      try {
        await invoke('ai_setup_save', { config: legacy });
        cache = legacy;
        dropLocal();
      } catch {
        // migrasi gagal: salinan lama dipertahankan agar bisa dicoba lagi pada peluncuran berikut
        cache = legacy;
      }
    }
    return cache;
  })();
  return loaded;
}

/** Simpan setup yang SUDAH tervalidasi. Backend memvalidasi ulang; kegagalan kembali sebagai kode aman. */
export async function persistSetup(
  setup: ProviderSetup,
): Promise<{ ok: true } | { ok: false; kind: StoreErrorKind }> {
  if (!isInTauri()) {
    try {
      globalThis.localStorage.setItem(SETUP_STORAGE_KEY, JSON.stringify(setup));
      return { ok: true };
    } catch {
      return { ok: false, kind: 'storage_unavailable' };
    }
  }
  try {
    await invoke('ai_setup_save', { config: setup });
  } catch (error) {
    return { ok: false, kind: backendKind(error) };
  }
  cache = setup;
  loaded = Promise.resolve(setup);
  dropLocal();
  return { ok: true };
}

/** Hapus setup tersimpan (backend dan salinan lama). */
export async function removeSetup(): Promise<{ ok: true } | { ok: false; kind: StoreErrorKind }> {
  if (isInTauri()) {
    try {
      await invoke('ai_setup_clear');
    } catch (error) {
      return { ok: false, kind: backendKind(error) };
    }
    cache = undefined;
    loaded = Promise.resolve(undefined);
  }
  dropLocal();
  return { ok: true };
}

/** Test hook. */
export function resetSetupStoreForTests(): void {
  cache = undefined;
  loaded = undefined;
}
