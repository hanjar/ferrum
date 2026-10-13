import { invoke } from '@tauri-apps/api/core';
import { aiChatHost } from './aiChatHost.svelte';
import { reloadAiRuntime } from './aiRuntime';
import {
  currentSetup,
  loadSetup,
  persistSetup,
  removeSetup,
  SETUP_STORAGE_KEY,
  type StoreErrorKind,
} from './providerSetupStore';
import { validateProviderSetup, type ProviderSetup } from './provider-setup';
import { RemoteProviderError } from './remote-provider';

// Setup provider AI yang TIDAK rahasia (endpoint, model, timeout, mode structured) dan pembungkus
// command credential (FER-412). Setup disimpan di backend (FER-430, lihat providerSetupStore.ts);
// key tidak pernah disimpan di sini: ia hanya lewat argumen `credentialSave` ke backend Rust,
// yang menulisnya ke penyimpanan credential OS.

export { SETUP_STORAGE_KEY };
/** Target credential tetap di backend; bukan rahasia. */
export const CREDENTIAL_REF = 'Ferrum/AI/custom';

export interface SetupForm {
  endpoint: string;
  model: string;
  timeoutMs: number;
  structured: ProviderSetup['structured'];
}

export const DEFAULT_SETUP_FORM: SetupForm = {
  endpoint: '',
  model: '',
  timeoutMs: 30_000,
  structured: 'json_object',
};

export type SaveSetupResult =
  { ok: true; setup: ProviderSetup } | { ok: false; kind: StoreErrorKind };

/** Setup tersimpan yang valid (tanpa fallback env), untuk prefill form. Sinkron: baca cache. */
export function loadStoredSetup(): ProviderSetup | undefined {
  return currentSetup();
}

/**
 * Muat setup dari backend (sekali, termasuk migrasi salinan browser lama). Bila hasilnya mengubah
 * setup yang dipakai runtime, runtime dibuang dan panel chat yang terbuka dibangun ulang
 * (riwayat dibawa). Dipanggil saat startup dan oleh Settings > AI sebelum prefill.
 */
export async function initProviderSetup(): Promise<ProviderSetup | undefined> {
  const before = currentSetup();
  const setup = await loadSetup();
  if (JSON.stringify(before) !== JSON.stringify(setup)) {
    reloadAiRuntime();
    aiChatHost.runtimeVersion++;
  }
  return setup;
}

/** Validasi + simpan + buang runtime ter-cache. Field di luar form diabaikan (tidak pernah disimpan). */
export async function saveProviderSetup(form: SetupForm): Promise<SaveSetupResult> {
  let setup: ProviderSetup;
  try {
    setup = validateProviderSetup({
      endpoint: form.endpoint,
      model: form.model,
      timeoutMs: form.timeoutMs,
      structured: form.structured,
      credentialRef: CREDENTIAL_REF,
    });
  } catch (error) {
    return {
      ok: false,
      kind:
        error instanceof RemoteProviderError && error.code !== 'backend_failure'
          ? error.code
          : 'invalid_config',
    };
  }
  const stored = await persistSetup(setup);
  if (!stored.ok) return stored;
  reloadAiRuntime();
  return { ok: true, setup };
}

export async function clearProviderSetup(): Promise<
  { ok: true } | { ok: false; kind: StoreErrorKind }
> {
  const removed = await removeSetup();
  if (removed.ok) reloadAiRuntime();
  return removed;
}

export type CredentialErrorKind =
  'invalid_value' | 'unsupported' | 'credential_failure' | 'test_profile';

/** FER-457: true when the app runs with a test profile (FERRUM_PROFILE_DIR); the real key is then read-only. */
export async function testProfileActive(): Promise<boolean> {
  try {
    return (await invoke<boolean>('test_profile_active')) === true;
  } catch {
    return false; // browser dev / old backend: no test profile
  }
}
export type CredentialResult =
  { ok: true; configured: boolean } | { ok: false; kind: CredentialErrorKind };

// Hanya `kind` dari backend yang diteruskan; pesan galat lain (yang bisa memuat nilai) dibuang.
function credentialKind(error: unknown): CredentialErrorKind {
  const kind = error && typeof error === 'object' ? (error as { kind?: unknown }).kind : undefined;
  return kind === 'invalid_value' || kind === 'unsupported' || kind === 'test_profile'
    ? kind
    : 'credential_failure';
}

async function credentialCall(
  command: string,
  args?: Record<string, unknown>,
): Promise<CredentialResult> {
  try {
    const result = await invoke<{ configured?: unknown }>(command, args);
    return { ok: true, configured: result?.configured === true };
  } catch (error) {
    return { ok: false, kind: credentialKind(error) };
  }
}

export const credentialStatus = (): Promise<CredentialResult> =>
  credentialCall('ai_credential_status');

export async function credentialSave(value: string): Promise<CredentialResult> {
  const result = await credentialCall('ai_credential_save', { value });
  if (result.ok) reloadAiRuntime();
  return result;
}

export async function credentialClear(): Promise<CredentialResult> {
  const result = await credentialCall('ai_credential_clear');
  if (result.ok) reloadAiRuntime();
  return result;
}
