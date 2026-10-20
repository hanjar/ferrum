// AI-02 / FER-181: default wiring of the capability table. This is the ONE place
// that says which engine gates are open. The integrator (not this module) hands in
// the context lane objects and mounts the facade in the app.
import { getAction } from '../../actions';
import {
  CommandFacade,
  type MutatingCapabilityId,
  type CommandAdapter,
  type FacadeOptions,
} from '../commands';
import {
  CapabilityRegistry,
  INTENT_SCHEMAS,
  type ActionLookup,
  type CapabilityDefinition,
  type EngineGate,
} from '../commandsRegistry';
import { createArchiveAdapter, type ArchiveAdapterDeps } from './archive';
import { createDeleteAdapter, DELETE_UNAVAILABLE_REASON, type DeleteAdapterDeps } from './delete';
import { createSyncAdapter, type SyncAdapterDeps } from './sync';

/**
 * Engine gates. `directories.sync` is READY (FER-168): it needs everything below, each proven by tests
 * (`docs/ai/sync-chat.md`) - the AI-10 engine semantics (first-run union, persisted baseline, no newer-wins, no
 * delete from chat), the verified baseline settlement (FER-414), and the backend approval guard for two-root
 * scopes (`src-tauri/src/ai/approval*.rs`; without it a sync plan cannot even be approved, fail closed).
 * Reverting this one entry closes the capability again: the registry then blocks prepare and execute and the
 * provider is no longer offered it.
 */
export const ENGINE_GATES: Record<MutatingCapabilityId, EngineGate> = {
  'archive.create': { status: 'ready' },
  'directories.sync': { status: 'ready' },
  // FER-576: Recycle Bin only, behind a warning card; see adapters/delete.ts and docs/ai/chain.md.
  'items.delete': { status: 'ready' },
};

export interface CapabilityWiring {
  archive: ArchiveAdapterDeps;
  sync: SyncAdapterDeps;
  /**
   * FER-576: Recycle Bin delete of the pane selection. Absent = the capability is closed (not offered, prepare
   * and execute are blocked), which is what tests and non-desktop builds get.
   */
  delete?: DeleteAdapterDeps;
  /** Defaults to the real action registry. */
  actions?: ActionLookup;
  /** Test/integration override of the gate table. */
  gates?: Partial<Record<MutatingCapabilityId, EngineGate>>;
}

export const PACK_UNAVAILABLE_REASON =
  'Layanan Pack (pembuat ZIP) tidak tersedia di jendela ini; arsip dari chat hanya berjalan di aplikasi desktop Ferrum.';

/** A ready archive gate stays ready only while the Pack service itself is reachable (FER-206). */
function packGate(gate: EngineGate, packAvailable: (() => boolean) | undefined): EngineGate {
  if (gate.status !== 'ready' || !packAvailable || packAvailable()) return gate;
  return { status: 'pending', reason: PACK_UNAVAILABLE_REASON };
}

/** Stand-in when no Recycle Bin port is wired: the gate is closed, so this is never reached. */
function unavailableDeleteAdapter(): CommandAdapter {
  const blocked = {
    ok: false as const,
    error: { code: 'blocked_capability' as const, message: DELETE_UNAVAILABLE_REASON },
  };
  return {
    id: 'items.delete',
    prepare: () => Promise.resolve(blocked),
    run: () => Promise.resolve(blocked),
  };
}

export function createCapabilityRegistry(wiring: CapabilityWiring): CapabilityRegistry {
  const gates = { ...ENGINE_GATES, ...wiring.gates };
  const definitions: CapabilityDefinition[] = [
    {
      id: 'archive.create',
      label: 'Buat arsip ZIP dari item panel',
      actionIds: ['file.pack'],
      engine: packGate(gates['archive.create'], wiring.archive.packAvailable),
      schema: INTENT_SCHEMAS['archive.create'],
      adapter: createArchiveAdapter(wiring.archive) as CommandAdapter,
    },
    {
      id: 'items.delete',
      label: 'Pindahkan item terpilih ke Recycle Bin',
      actionIds: ['file.delete'],
      engine: wiring.delete
        ? gates['items.delete']
        : { status: 'pending', reason: DELETE_UNAVAILABLE_REASON },
      schema: INTENT_SCHEMAS['items.delete'],
      adapter: (wiring.delete
        ? createDeleteAdapter(wiring.delete)
        : unavailableDeleteAdapter()) as CommandAdapter,
    },
    {
      id: 'directories.sync',
      label: 'Sinkronkan kedua panel dua arah',
      actionIds: ['commands.synchronize'],
      engine: gates['directories.sync'],
      schema: INTENT_SCHEMAS['directories.sync'],
      adapter: createSyncAdapter(wiring.sync) as CommandAdapter,
    },
  ];
  return new CapabilityRegistry(definitions, wiring.actions ?? getAction);
}

export function createCommandFacade(
  wiring: CapabilityWiring,
  options?: FacadeOptions,
): { facade: CommandFacade; registry: CapabilityRegistry } {
  const registry = createCapabilityRegistry(wiring);
  return { facade: new CommandFacade(registry, options), registry };
}
