/**
 * Shell-launch IPC (F2-25 + F2-27 print verb). Thin typed wrappers over
 * `src-tauri/src/shell_open.rs`; the Rust side launches through structured
 * platform APIs (ShellExecute verbs / argv arrays) — no shell strings.
 */
import { invoke } from '@tauri-apps/api/core';
import { FsError } from './types';
import { isInTauri } from './tauri';
import { toast } from '../stores/toastStore.svelte';
import { dialogs } from '../stores/dialogStore.svelte';

/** Persisted per-OS terminal preference (`shell_open::TerminalPref`). */
export type TerminalChoice = 'auto' | 'wt' | 'cmd' | 'powershell';

export const TERMINAL_CHOICES: { value: TerminalChoice; label: string }[] = [
  { value: 'auto', label: 'Auto' },
  { value: 'wt', label: 'Windows Terminal' },
  { value: 'cmd', label: 'Command Prompt' },
  { value: 'powershell', label: 'PowerShell' },
];

function unsupported(): FsError {
  return new FsError({
    code: 'unsupported',
    message: 'Launching applications requires the desktop app (not browser dev)',
  });
}

async function launch(command: string, args: Record<string, unknown>): Promise<void> {
  if (!isInTauri()) throw unsupported();
  try {
    await invoke(command, args);
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

/** Open a file or folder with its OS default association. */
export function openExternal(path: string): Promise<void> {
  return launch('open_external', { path });
}

/**
 * Open the third-party notices installed with the app (FER-554). The backend fixes the file, so the
 * webview passes no path.
 */
export function openThirdPartyNotices(): Promise<void> {
  return launch('open_third_party_notices', {});
}

/**
 * Open the OS "Associate With…" picker for one file. The user decides in
 * the picker; Ferrum never changes a system association by itself.
 */
export function associateWith(path: string): Promise<void> {
  return launch('associate_with', { path });
}

/**
 * Open the preferred terminal at `cwd`. A missing executable arrives as a
 * structured FsError (notFound) so the UI can prompt.
 */
export function openTerminal(cwd: string, program: TerminalChoice = 'auto'): Promise<void> {
  return launch('open_terminal', { cwd, program });
}

/** Print via the associated application's print verb (F2-27). */
export function printFile(path: string): Promise<void> {
  return launch('print_file', { path });
}

export type ShellErrorOutcome =
  { kind: 'silent' } | { kind: 'toast'; message: string } | { kind: 'error'; message: string };

/**
 * FER-513 UI error mapping:
 * - Cancelled: silent (user deliberately declined native confirmation)
 * - Busy: toast "Konfirmasi lain masih terbuka" (another confirmation is open)
 * - Other: honest error dialog
 */
export function mapShellError(err: unknown): ShellErrorOutcome {
  if (err instanceof FsError) {
    if (err.code === 'cancelled') {
      return { kind: 'silent' };
    }
    if (err.code === 'busy') {
      return { kind: 'toast', message: 'Konfirmasi lain masih terbuka' };
    }
  }
  const message = err instanceof Error ? err.message : String(err);
  return { kind: 'error', message };
}

export function handleShellError(title: string, err: unknown): void {
  const mapped = mapShellError(err);
  if (mapped.kind === 'silent') return;
  if (mapped.kind === 'toast') {
    toast.show(mapped.message);
    return;
  }
  dialogs.open({ kind: 'error', title, message: mapped.message });
}
