// AI-02 / FER-181: capability registry. ONE definition table per capability
// holds its engine gate, the UI actions it mirrors, the model-facing JSON schema
// and the adapter; availability, the disabled reason and the adapter handed to
// the facade are all derived from that same entry (no second list to drift).
//
// A capability is active only when (a) its engine gate is `ready` and (b) every
// UI action it mirrors exists in the action registry with a handler and no
// `disabledReason`. Pending => inactive => `adapterFor` returns a typed block, so
// neither prepare nor execute can reach the adapter.
import {
  err,
  ok,
  type Availability,
  type MutatingCapabilityId,
  type CapabilitySource,
  type CommandAdapter,
  type Result,
} from './commands';

export interface EngineGate {
  status: 'ready' | 'pending';
  /** Required when pending: why the capability is off (shown to the user). */
  reason?: string;
}

/** Minimal view of an `Action` (src/lib/actions.ts) needed for availability. */
export interface ActionLike {
  run?: unknown;
  disabledReason?: string;
  available?: unknown;
}
export type ActionLookup = (id: string) => ActionLike | undefined;

export interface IntentSchema {
  type: 'object';
  additionalProperties: false;
  required: string[];
  properties: Record<string, unknown>;
  /** Valid examples; tests assert `parseIntent` accepts each (lockstep). */
  examples: Array<Record<string, unknown>>;
}

const PANE_REF = { enum: ['left', 'right', 'activeAtSubmit'] };
const ENTRIES = { enum: ['all', 'filesOnly'] };

/** Model-facing schemas. Mirror `parseIntent` in contracts.ts (checked by tests). */
export const INTENT_SCHEMAS: Record<MutatingCapabilityId, IntentSchema> = {
  'archive.create': {
    type: 'object',
    additionalProperties: false,
    required: ['capability', 'sourcePane', 'selector', 'format', 'target'],
    properties: {
      capability: { const: 'archive.create' },
      sourcePane: PANE_REF,
      selector: {
        oneOf: [
          {
            type: 'object',
            additionalProperties: false,
            required: ['kind'],
            properties: { kind: { const: 'selected' } },
          },
          {
            type: 'object',
            additionalProperties: false,
            required: ['kind', 'entries'],
            properties: { kind: { const: 'allVisible' }, entries: ENTRIES },
          },
          {
            type: 'object',
            additionalProperties: false,
            required: ['kind', 'entries', 'includeHidden'],
            properties: {
              kind: { const: 'directoryContents' },
              entries: ENTRIES,
              includeHidden: { type: 'boolean' },
            },
          },
        ],
      },
      format: { enum: ['zip'] },
      target: {
        oneOf: [
          {
            type: 'object',
            additionalProperties: false,
            required: ['kind'],
            properties: { kind: { const: 'sourceDirectory' } },
          },
          {
            type: 'object',
            additionalProperties: false,
            required: ['kind', 'pane'],
            properties: { kind: { const: 'pane' }, pane: PANE_REF },
          },
        ],
      },
      outputName: { type: 'string', maxLength: 128, description: 'Basename only.' },
    },
    examples: [
      {
        capability: 'archive.create',
        sourcePane: 'left',
        selector: { kind: 'allVisible', entries: 'all' },
        format: 'zip',
        target: { kind: 'sourceDirectory' },
      },
      {
        capability: 'archive.create',
        sourcePane: 'activeAtSubmit',
        selector: { kind: 'selected' },
        format: 'zip',
        target: { kind: 'pane', pane: 'right' },
        outputName: 'invoice-september.zip',
      },
    ],
  },
  'directories.sync': {
    type: 'object',
    additionalProperties: false,
    required: ['capability', 'leftPane', 'rightPane', 'mode'],
    properties: {
      capability: { const: 'directories.sync' },
      leftPane: { const: 'left' },
      rightPane: { const: 'right' },
      mode: { const: 'bidirectional' },
    },
    examples: [
      {
        capability: 'directories.sync',
        leftPane: 'left',
        rightPane: 'right',
        mode: 'bidirectional',
      },
    ],
  },
};

export interface CapabilityDefinition {
  id: MutatingCapabilityId;
  label: string;
  /** UI action ids this capability mirrors (must be enabled in the action registry). */
  actionIds: readonly string[];
  engine: EngineGate;
  schema: IntentSchema;
  adapter: CommandAdapter;
}

/** What the provider/prompt may see: no adapter, no functions. */
export interface CapabilityManifestEntry {
  id: MutatingCapabilityId;
  label: string;
  available: boolean;
  reason?: string;
  schema: IntentSchema;
}

const UNKNOWN: Availability = {
  available: false,
  code: 'unknown_capability',
  reason: 'Capability tidak dikenal.',
};

export class CapabilityRegistry implements CapabilitySource {
  private readonly byId = new Map<string, CapabilityDefinition>();

  constructor(
    definitions: readonly CapabilityDefinition[],
    private readonly actions: ActionLookup,
  ) {
    for (const d of definitions) this.byId.set(d.id, d);
  }

  availability(id: string): Availability {
    const def = this.byId.get(id);
    if (!def) return UNKNOWN;
    if (def.engine.status !== 'ready') {
      return {
        available: false,
        code: 'pending',
        reason: def.engine.reason ?? 'Fitur belum tersedia pada versi ini.',
      };
    }
    for (const actionId of def.actionIds) {
      const action = this.actions(actionId);
      if (!action) {
        return {
          available: false,
          code: 'action_missing',
          reason: `Aksi UI "${actionId}" tidak ditemukan.`,
        };
      }
      // `available()` (per-selection context of the manual action) is deliberately
      // not consulted: chat resolves its own scope. Only static enablement counts.
      if (!action.run || action.disabledReason) {
        return {
          available: false,
          code: 'action_disabled',
          reason: action.disabledReason ?? `Aksi UI "${actionId}" belum aktif.`,
        };
      }
    }
    return { available: true };
  }

  adapterFor(id: string): Result<CommandAdapter> {
    const a = this.availability(id);
    if (!a.available) return err('blocked_capability', a.reason, a.code);
    return ok(this.byId.get(id)!.adapter);
  }

  /**
   * Ids to offer the provider for this request: exactly the capabilities that are ready and enabled
   * right now. When an engine gate opens (or an action becomes available) the capability appears
   * here on the next request without any change to the prompt or the provider.
   */
  offeredCapabilityIds(): MutatingCapabilityId[] {
    return this.describe()
      .filter((entry) => entry.available)
      .map((entry) => entry.id);
  }

  describe(): CapabilityManifestEntry[] {
    return [...this.byId.values()].map((def) => {
      const a = this.availability(def.id);
      const entry: CapabilityManifestEntry = {
        id: def.id,
        label: def.label,
        available: a.available,
        schema: def.schema,
      };
      if (!a.available) entry.reason = a.reason;
      return entry;
    });
  }
}
