// Composition root for the AI assistant (FER-382): wires the context lane, capability registry,
// facade, executor, provider, ledger and queue to the real stores. Built lazily on first use so the
// app pays nothing until the chat is opened. Nothing here persists chat history or any secret.
import { packServiceAvailable } from '../packService';
import { activity } from '../stores/activityStore.svelte';
import { ops } from '../stores/opsStore.svelte';
import { refreshAllPanes } from '../stores/panesRegistry.svelte';
import { ui } from '../uiState.svelte';
import { tauriActivityQueryPort } from './activityQuery';
import { duWorkspace } from '../diskUsage/duWorkspace.svelte';
import { diskUsageQueryChildren, diskUsageTopFiles } from '../ipc/diskUsage';
import { diskUsageWorkspacePort } from './diskUsageQuery';
import { copyControlSnapshot, type CopyControlState } from '../ipc/copyControl';
import type { CopyControlPort } from './copyAdjust';
import { indexerSetMode, indexerStatus } from '../ipc/indexer';
import { isInTauri } from '../ipc/tauri';
import type { IndexerControlPort } from './indexerControl';
import { createCommandFacade } from './adapters';
import { SyncPreviewStore } from './adapters/sync';
import { backendIntentTransport } from './backendBridge';
import { tauriAttachmentAsker } from './attachments';
import { tauriAgentAsker } from './agentCli.svelte';
import { createBackendGuard } from './backendGuard';
import { ChatStore } from './chatStore.svelte';
import { ContextService } from './context';
import { ContextFlow } from './contextFlow';
import { registryHost } from './contextPanes';
import { ScopeResolver } from './contextResolver';
import { AiExecutor } from './executor';
import { backendExecutionLedger } from './executorBridge';
import { PollingQueuePort } from './executorQueue';
import type { IntentProvider, ProviderEvent } from './provider';
import { validateProviderSetup, type ProviderSetup } from './provider-setup';
import { currentSetup } from './providerSetupStore';
import { providerStatus } from './providerStatus.svelte';
import {
  RemoteIntentProvider,
  RemoteProviderError,
  type RemoteProviderErrorCode,
} from './remote-provider';

// The credential itself lives in the OS store under this fixed target; it is never part of setup.
const CREDENTIAL_REF = 'Ferrum/AI/custom';

/**
 * Non-secret provider setup: endpoint + model (+ timeout). Order: the stored setup (backend cache
 * in the desktop app since FER-430; browser storage only outside Tauri), then build-time
 * `VITE_FERRUM_AI_ENDPOINT` / `VITE_FERRUM_AI_MODEL`. Returns undefined when nothing valid is configured.
 */
export function loadProviderSetup(): ProviderSetup | undefined {
  const stored: unknown = currentSetup();
  const env = import.meta.env as Record<string, string | undefined>;
  const candidates: unknown[] = [
    stored,
    env.VITE_FERRUM_AI_ENDPOINT && env.VITE_FERRUM_AI_MODEL
      ? {
          endpoint: env.VITE_FERRUM_AI_ENDPOINT,
          model: env.VITE_FERRUM_AI_MODEL,
          timeoutMs: 30_000,
          credentialRef: CREDENTIAL_REF,
          structured: 'none',
        }
      : undefined,
  ];
  for (const candidate of candidates) {
    if (candidate === undefined) continue;
    try {
      return validateProviderSetup(candidate);
    } catch {
      // try the next source
    }
  }
  return undefined;
}

providerStatus.useSetupSource(loadProviderSetup);

/** Stand-in used until a provider is configured: every plan fails with a clear code. */
class UnconfiguredProvider implements IntentProvider {
  plan(): AsyncIterable<ProviderEvent> {
    return {
      [Symbol.asyncIterator]: () => ({
        next: () =>
          Promise.reject(
            new RemoteProviderError('invalid_config', 'AI provider is not configured'),
          ),
      }),
    };
  }
}

export interface AiRuntime {
  /** Stops the queue polling timer; called when the runtime is replaced. */
  dispose(): void;
  chat: ChatStore;
  executor: AiExecutor;
  configured: boolean;
}

let runtime: AiRuntime | undefined;
/** Chat of the runtime that was dropped by `reloadAiRuntime`; its history moves to the next runtime. */
let carriedChat: ChatStore | undefined;

/** FER-471: the executor's view of the running copy job, backed by the same store as the progress dialog. */
function opsCopyControlPort(): CopyControlPort {
  return {
    available: () => [...ops.copyControls.values()].some((c) => c.running),
    running: async (): Promise<CopyControlState[]> => copyControlSnapshot(),
    adjust: (operationId, change) => ops.adjustCopy(operationId, change),
  };
}

/**
 * FER-476: FerrumIndexer status and mode change. Offered whenever the desktop app runs (the service
 * only exists on Windows); the mode write still needs the card's confirm and, when raising, UAC.
 */
function indexerControlPort(): IndexerControlPort {
  return {
    available: () => isInTauri(),
    status: () => indexerStatus(),
    setMode: (mode, confirmed) => indexerSetMode(mode, confirmed),
  };
}

export function createAiRuntime(setup: ProviderSetup | undefined = loadProviderSetup()): AiRuntime {
  const host = registryHost(() => (ui.activePane === 0 ? 'left' : 'right'));
  const ctx = new ContextService(host);
  const resolver = new ScopeResolver(ctx);
  const flow = new ContextFlow(ctx, resolver);

  const outputExists = (pane: 'left' | 'right', name: string): boolean => {
    const lower = name.toLowerCase();
    return (
      host
        .probe(pane)
        ?.allEntries()
        .some((entry) => entry.name.toLowerCase() === lower) ?? false
    );
  };
  // The same port serves the model's history questions and the archive adapter's `entity` selector
  // (FER-433): `locate` is a read-only identity lookup that answers nothing in a private session.
  const activityPort = tauriActivityQueryPort({
    available: () => activity.available,
    disclosureAcked: () => activity.disclosureAcked,
    privateSession: () => activity.privateSession,
  });
  const queue = new PollingQueuePort({ read: () => ops.all, cancel: (id) => ops.cancel(id) });
  queue.start(250);
  // Local previews of sync plans (items, conflicts, backend guard scope); never sent to the provider.
  const syncPreviews = new SyncPreviewStore();
  // Settlement of the sync baseline arrives after the chat exists; the card shows it (not only diagnostics).
  const live: { chat?: ChatStore } = {};
  const { facade, registry } = createCommandFacade({
    archive: {
      flow,
      resolver,
      ctx,
      outputExists,
      onCompleted: refreshAllPanes,
      packAvailable: packServiceAvailable,
      locateEntity: (entityId) => activityPort.locate?.(entityId) ?? Promise.resolve(null),
    },
    // `readJob`: the queue port the executor uses; `settleSyncRun` is the only baseline writer (FER-414).
    sync: {
      flow,
      ctx,
      store: syncPreviews,
      readJob: (jobId) => queue.get(jobId),
      onSettled: (result, previewKey) => live.chat?.noteSettlement(previewKey, result),
    },
  });

  const provider: IntentProvider = setup
    ? new RemoteIntentProvider(setup, backendIntentTransport)
    : new UnconfiguredProvider();
  const executor = new AiExecutor({
    provider,
    capture: (requestId) => ctx.capture(requestId),
    modelContext: (snapshot) => ctx.modelContext(snapshot),
    facade,
    capabilities: registry,
    ledger: backendExecutionLedger,
    queue,
    // FER-465: host:port for the error text; a failed plan also turns the header indicator red.
    providerEndpoint: () => setup?.endpoint,
    onProviderFailure: (_code, kind) => {
      if (['unreachable', 'network', 'timeout', 'auth', 'server_error'].includes(kind))
        providerStatus.noteFailure(kind as RemoteProviderErrorCode);
    },
    // Backend re-check right before writing (FER-410): fingerprints at preview, authorizes at execute.
    guard: createBackendGuard({
      sourcePaths: (plan) =>
        plan.operation.kind === 'archive.create'
          ? resolver.entries(plan.operation.sourceManifest).map((entry) => entry.path)
          : undefined,
      syncScope: (plan) => syncPreviews.scopeOf(plan),
    }),
    // Item list of the preview card: read from the frozen manifest of the prepared plan (local;
    // never sent to the provider). Only archive plans carry a source manifest.
    // Sync conflicts are only LISTED (they are skipped); chat never decides them, so no conflict port is wired.
    conflictViews: (plan) => syncPreviews.conflictsOf(plan),
    listItems: async (plan, offset, limit) => {
      if (plan.operation.kind === 'directories.sync') return syncPreviews.page(plan, offset, limit);
      if (plan.operation.kind !== 'archive.create') return { rows: [], total: 0 };
      const entries = resolver.entries(plan.operation.sourceManifest);
      return {
        rows: entries.slice(offset, offset + limit).map((entry) => ({
          ref: entry.path,
          label: entry.name,
          detail: entry.isDir ? 'folder' : 'file',
        })),
        total: entries.length,
      };
    },
    // ACT-08: read-only history questions. Offered to the model only while the activity service is
    // open AND the first-run disclosure was acknowledged (FER-432); this window's private session
    // answers `withheld` without reading. Results stay local.
    activity: activityPort,
    // FER-471: tune the running copy (threads/priority). Offered only while a copy runs; the write
    // happens after the user's confirm on the chat card and reuses the progress dialog's command.
    copyControl: opsCopyControlPort(),
    // FER-476: "is the index ready?" (read) and the privacy-mode switch (always behind a card).
    indexer: indexerControlPort(),
    // FER-466: questions about the Disk Usage result on screen ("paling gede itu apa"). Read-only and
    // answered locally on a card; no scan data (path, name, size) is ever put in a provider request.
    diskUsage: diskUsageWorkspacePort(duWorkspace, {
      topFiles: (scanId, nodeId, limit, filter) => diskUsageTopFiles(scanId, nodeId, limit, filter),
      children: (scanId, nodeId, limit, filter) =>
        diskUsageQueryChildren(scanId, nodeId, 0, limit, 'largestFirst', filter),
    }),
  });
  void executor.recover();
  // FER-486: "Add to chat" lampiran (desktop app saja; isi file dibaca backend saat Kirim).
  // FER-490: "Ask AI agent" (agent CLI lokal, mode baca-saja; desktop app saja).
  const chat = new ChatStore(
    executor,
    isInTauri() ? tauriAttachmentAsker(() => setup) : undefined,
    isInTauri() ? tauriAgentAsker() : undefined,
  );
  live.chat = chat;
  return {
    chat,
    executor,
    configured: setup !== undefined,
    dispose: () => queue.stop(),
  };
}

/** The app-wide runtime (created on first call). */
export function aiRuntime(): AiRuntime {
  if (!runtime) {
    runtime = createAiRuntime();
    // Keep the conversation across a provider change (the new runtime only brings the new provider).
    if (carriedChat) runtime.chat.adoptHistory(carriedChat);
    carriedChat = undefined;
  }
  return runtime;
}

/** Test hook. */
export function resetAiRuntimeForTests(): void {
  runtime = undefined;
  carriedChat = undefined;
}

/**
 * Buang runtime ter-cache agar provider dibuat ulang dari setup terbaru (dipanggil setelah
 * setup/credential diubah di Settings). Chat yang sedang berjalan di runtime lama tidak diubah;
 * runtime berikutnya dibuat saat `aiRuntime()` dipanggil lagi.
 */
export function reloadAiRuntime(): void {
  if (runtime) carriedChat = runtime.chat;
  runtime?.dispose();
  runtime = undefined;
}
