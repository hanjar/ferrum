// FER-486 "Add to chat": item terpilih (atau item di kursor) di panel aktif -> lampiran chat AI.
// Ringan: runtime AI dimuat lazily saat aksi dijalankan, jadi registry aksi tidak ikut memuatnya.
import { activePane } from '../stores/panesRegistry.svelte';
import { aiChatHost } from './aiChatHost.svelte';

export interface AddToChatTarget {
  path: string;
  name: string;
  isDir: boolean;
}

function baseName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, '');
  return trimmed.slice(Math.max(trimmed.lastIndexOf('\\'), trimmed.lastIndexOf('/')) + 1);
}

/** Item terpilih di panel aktif; tanpa pilihan: item di kursor. Entri ".." tidak pernah ikut. */
export function addToChatTargets(): AddToChatTarget[] {
  const pane = activePane();
  if (!pane) return [];
  const selected = pane.selectedPaths();
  if (selected.length) {
    const visible = new Map((pane.visibleEntries?.() ?? []).map((e) => [e.path, e]));
    return selected.map((path) => {
      const entry = visible.get(path);
      return { path, name: entry?.name ?? baseName(path), isDir: entry?.isDir ?? false };
    });
  }
  const cursor = pane.cursorEntry();
  if (!cursor || cursor.name === '..') return [];
  return [{ path: cursor.path, name: cursor.name, isDir: cursor.isDir }];
}

/** Buka panel chat dan tambahkan target sebagai chip lampiran (maks. 10; sisanya dicatat di chat). */
export async function addSelectionToChat(
  targets: AddToChatTarget[] = addToChatTargets(),
): Promise<void> {
  if (!targets.length) return;
  const { aiRuntime } = await import('./aiRuntime');
  if (!aiChatHost.open) aiChatHost.toggle();
  await aiRuntime().chat.addAttachments(targets);
}

/**
 * FER-490 "Ask AI agent about selection": lampirkan seperti Add to chat, lalu arahkan composer ke
 * agent CLI default (agent membaca file sendiri dalam mode baca-saja). Tanpa agent yang didukung:
 * catatan di chat, tujuan tetap AI Ferrum.
 */
export async function askAgentAboutSelection(
  targets: AddToChatTarget[] = addToChatTargets(),
): Promise<void> {
  if (!targets.length) return;
  const [{ aiRuntime }, { agentCli }] = await Promise.all([
    import('./aiRuntime'),
    import('./agentCli.svelte'),
  ]);
  if (!aiChatHost.open) aiChatHost.toggle();
  const chat = aiRuntime().chat;
  await Promise.all([chat.addAttachments(targets), agentCli.load()]);
  const agent = chat.canUseAgents ? agentCli.defaultAgent : undefined;
  if (agent) chat.setTarget(agent.id);
  else chat.notice('Tidak ada agent CLI yang didukung. Lihat Settings → AI → Agent CLI.');
}
