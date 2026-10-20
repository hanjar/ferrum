// FER-410 / AI-SEC-1: the TypeScript half of the backend approval guard. The Rust `ApprovalLedger`
// fingerprints the sources when the preview is built and re-checks them (plus the target and the output
// name) right before the executor writes. Only ids, digests and LOCAL paths cross this boundary; the
// rejection data carries stable codes and a short summary, never a raw path.
import { invoke } from '@tauri-apps/api/core';
import type { ApprovalTicket } from './approval';
import type { ResolvedPlan } from './contracts';
import type { ExecutionGuard } from './executor';

/**
 * FER-168: what the backend must fingerprint for one approved sync plan. LOCAL data (paths never go to the
 * provider): both roots, every file that will be copied or overwritten (sources and existing targets) plus
 * the directories that contain them, and the targets of NEW copies that must still not exist at run time.
 */
export interface SyncGuardScope {
  leftRoot: string;
  rightRoot: string;
  sourcePaths: readonly string[];
  absentPaths: readonly string[];
}

export interface BackendGuardDeps {
  /** Frozen source paths of the prepared plan (local manifest; undefined = not available). */
  sourcePaths(plan: ResolvedPlan): readonly string[] | undefined;
  /** Sync scope of the prepared plan; undefined = not available (sync then fails closed). */
  syncScope?(plan: ResolvedPlan): SyncGuardScope | undefined;
}

interface GuardFailure {
  kind?: unknown;
  reasons?: unknown;
  summary?: unknown;
}

const FALLBACK = 'Validasi backend gagal; tidak ada yang disetujui atau ditulis.';

function messageOf(error: unknown): string {
  if (error && typeof error === 'object' && !(error instanceof Error)) {
    const { summary } = error as GuardFailure;
    if (typeof summary === 'string' && summary.length > 0 && summary.length <= 300) return summary;
  }
  return FALLBACK;
}

export function createBackendGuard(deps: BackendGuardDeps): ExecutionGuard {
  // handleId -> scope digest the backend computed from ITS OWN fingerprints at registration.
  const scopes = new Map<string, string>();

  async function registerRequest(
    ticket: ApprovalTicket,
    scope: Record<string, unknown>,
  ): Promise<{ ok: true } | { ok: false; message: string }> {
    try {
      const registered = await invoke<{ scopeDigest?: unknown }>('ai_register_approval', {
        request: {
          handleId: ticket.handleId,
          planId: ticket.planId,
          planDigest: ticket.planDigest,
          revision: ticket.revision,
          ...scope,
          expiresAtMs: ticket.expiresAtMs,
        },
      });
      if (typeof registered?.scopeDigest !== 'string' || registered.scopeDigest.length === 0) {
        return { ok: false, message: FALLBACK };
      }
      scopes.set(ticket.handleId, registered.scopeDigest);
      return { ok: true };
    } catch (error) {
      return { ok: false, message: messageOf(error) };
    }
  }

  return {
    async register(ticket: ApprovalTicket, plan: ResolvedPlan) {
      const op = plan.operation;
      if (op.kind === 'directories.sync') {
        const scope = deps.syncScope?.(plan);
        // Fail closed: the scope must exist, be non-empty and describe exactly the roots of the plan.
        if (
          !scope ||
          scope.sourcePaths.length === 0 ||
          scope.leftRoot !== op.leftRoot ||
          scope.rightRoot !== op.rightRoot
        ) {
          return {
            ok: false as const,
            message: 'Item sinkronisasi tidak tersedia untuk divalidasi; tidak ada yang disetujui.',
          };
        }
        return registerRequest(ticket, {
          root: scope.leftRoot,
          secondRoot: scope.rightRoot,
          sourcePaths: [...scope.sourcePaths],
          absentPaths: [...scope.absentPaths],
        });
      }
      if (op.kind === 'items.delete') {
        // FER-576: the root is the folder the selection lives in; no output name. The backend re-checks every
        // item (fingerprint) right before the delete command runs.
        const items = deps.sourcePaths(plan);
        if (!items || items.length === 0) {
          return {
            ok: false as const,
            message:
              'Item yang akan dihapus tidak tersedia untuk divalidasi; tidak ada yang disetujui.',
          };
        }
        return registerRequest(ticket, {
          root: op.sourceDirectory,
          sourcePaths: [...items],
          outputName: null,
        });
      }
      const paths = deps.sourcePaths(plan);
      if (!paths || paths.length === 0) {
        return {
          ok: false as const,
          message: 'Item sumber tidak tersedia untuk divalidasi; tidak ada yang disetujui.',
        };
      }
      return registerRequest(ticket, {
        root: op.targetDirectory,
        sourcePaths: [...paths],
        outputName: op.outputName,
      });
    },

    async authorize(ticket: ApprovalTicket, plan: ResolvedPlan) {
      const itemScope = scopes.get(ticket.handleId);
      if (!itemScope) {
        return {
          ok: false as const,
          message: 'Persetujuan tidak terdaftar di backend; tidak ada yang ditulis.',
        };
      }
      try {
        await invoke('ai_authorize_execute', {
          claim: {
            handleId: ticket.handleId,
            planId: ticket.planId,
            planDigest: ticket.planDigest,
            revision: ticket.revision,
            // Sync: the two roots of the plan (the ticket's `left|right` label is not a path).
            ...(plan.operation.kind === 'directories.sync'
              ? { root: plan.operation.leftRoot, secondRoot: plan.operation.rightRoot }
              : { root: ticket.root }),
            itemScope,
          },
        });
        return { ok: true as const };
      } catch (error) {
        return { ok: false as const, message: messageOf(error) };
      }
    },

    revoke(ticket: ApprovalTicket) {
      scopes.delete(ticket.handleId);
      void Promise.resolve(invoke('ai_revoke_approval', { handleId: ticket.handleId })).catch(
        () => undefined,
      );
    },
  };
}
