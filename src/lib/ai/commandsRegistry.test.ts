// FER-181: one definition table drives availability, disabled reason, schema and
// adapter. Pending capabilities are inactive and block their adapter.
import { describe, expect, it, vi } from 'vitest';
import { parseIntent } from './contracts';
import type { CommandAdapter } from './commands';
import {
  CapabilityRegistry,
  INTENT_SCHEMAS,
  type ActionLookup,
  type CapabilityDefinition,
} from './commandsRegistry';

const fakeAdapter = (id: CommandAdapter['id']): CommandAdapter => ({
  id,
  prepare: vi.fn(),
  run: vi.fn(),
});

function defs(
  over: Partial<Record<string, Partial<CapabilityDefinition>>> = {},
): CapabilityDefinition[] {
  return [
    {
      id: 'archive.create',
      label: 'Buat arsip ZIP',
      actionIds: ['file.pack'],
      engine: { status: 'ready' },
      schema: INTENT_SCHEMAS['archive.create'],
      adapter: fakeAdapter('archive.create'),
      ...over['archive.create'],
    },
    {
      id: 'directories.sync',
      label: 'Sinkronkan dua panel',
      actionIds: ['commands.synchronize'],
      engine: { status: 'pending', reason: 'engine belum lengkap' },
      schema: INTENT_SCHEMAS['directories.sync'],
      adapter: fakeAdapter('directories.sync'),
      ...over['directories.sync'],
    },
  ];
}

const allActions: ActionLookup = () => ({ run: () => {} });

describe('availability', () => {
  it('ready engine + enabled UI action is available', () => {
    const r = new CapabilityRegistry(defs(), allActions);
    expect(r.availability('archive.create')).toEqual({ available: true });
  });

  it('a pending engine is unavailable with its reason, even when the UI action exists', () => {
    const r = new CapabilityRegistry(defs(), allActions);
    expect(r.availability('directories.sync')).toEqual({
      available: false,
      code: 'pending',
      reason: 'engine belum lengkap',
    });
  });

  it('reads the UI action registry: missing and disabled actions disable the capability', () => {
    const missing = new CapabilityRegistry(defs(), () => undefined);
    expect(missing.availability('archive.create')).toMatchObject({
      available: false,
      code: 'action_missing',
    });
    const disabled = new CapabilityRegistry(defs(), (id) =>
      id === 'file.pack' ? { disabledReason: 'Belum diimplementasikan' } : { run: () => {} },
    );
    expect(disabled.availability('archive.create')).toEqual({
      available: false,
      code: 'action_disabled',
      reason: 'Belum diimplementasikan',
    });
    const noHandler = new CapabilityRegistry(defs(), () => ({}));
    expect(noHandler.availability('archive.create')).toMatchObject({ code: 'action_disabled' });
  });

  it('does not depend on per-selection availability of the manual action', () => {
    // `Action.available()` (e.g. "has a selection") is not consulted: chat resolves its own scope.
    const lookup: ActionLookup = () => ({ run: () => {}, available: () => false });
    expect(new CapabilityRegistry(defs(), lookup).availability('archive.create')).toEqual({
      available: true,
    });
  });

  it('unknown capability ids are unavailable', () => {
    const r = new CapabilityRegistry(defs(), allActions);
    expect(r.availability('shell.exec')).toMatchObject({
      available: false,
      code: 'unknown_capability',
    });
    expect(r.availability('__proto__')).toMatchObject({
      available: false,
      code: 'unknown_capability',
    });
  });
});

describe('adapterFor', () => {
  it('returns the adapter for an available capability', () => {
    const d = defs();
    const r = new CapabilityRegistry(d, allActions);
    expect(r.adapterFor('archive.create')).toEqual({ ok: true, value: d[0]!.adapter });
  });

  it('blocks a pending capability: no adapter is handed out and the reason is reported', () => {
    const r = new CapabilityRegistry(defs(), allActions);
    expect(r.adapterFor('directories.sync')).toEqual({
      ok: false,
      error: { code: 'blocked_capability', message: 'engine belum lengkap', cause: 'pending' },
    });
  });

  it('re-evaluates on every call (a gate flipped later takes effect)', () => {
    const d = defs();
    const r = new CapabilityRegistry(d, allActions);
    expect(r.adapterFor('directories.sync').ok).toBe(false);
    d[1]!.engine = { status: 'ready' };
    expect(r.adapterFor('directories.sync').ok).toBe(true);
  });

  it('unknown ids are blocked, not thrown', () => {
    const r = new CapabilityRegistry(defs(), allActions);
    expect(r.adapterFor('shell.exec')).toMatchObject({
      ok: false,
      error: { code: 'blocked_capability', cause: 'unknown_capability' },
    });
  });
});

describe('describe() model-facing manifest', () => {
  it('lists every capability with availability, reason and schema from the same table', () => {
    const r = new CapabilityRegistry(defs(), allActions);
    const m = r.describe();
    expect(m.map((c) => c.id)).toEqual(['archive.create', 'directories.sync']);
    expect(m[0]).toMatchObject({ available: true, schema: INTENT_SCHEMAS['archive.create'] });
    expect(m[1]).toMatchObject({ available: false, reason: 'engine belum lengkap' });
  });

  it('exposes no adapter or function to the model (plain JSON only)', () => {
    const json = JSON.stringify(new CapabilityRegistry(defs(), allActions).describe());
    expect(json).not.toContain('prepare');
    expect(JSON.parse(json)).toHaveLength(2);
  });
});

describe('intent schemas stay in lockstep with parseIntent', () => {
  for (const id of ['archive.create', 'directories.sync'] as const) {
    const schema = INTENT_SCHEMAS[id];

    it(`${id}: strict object schema with a const capability`, () => {
      expect(schema.type).toBe('object');
      expect(schema.additionalProperties).toBe(false);
      expect(schema.properties.capability).toEqual({ const: id });
      expect(schema.required).toContain('capability');
      for (const key of schema.required) expect(Object.keys(schema.properties)).toContain(key);
    });

    it(`${id}: every schema example is accepted by the contract parser`, () => {
      expect(schema.examples.length).toBeGreaterThan(0);
      for (const intent of schema.examples) {
        const parsed = parseIntent({ schemaVersion: 1, requestId: 'req_1', intent }, 'req_1');
        expect(parsed.intent.capability).toBe(id);
      }
    });

    it(`${id}: an unknown extra property is rejected by the contract parser`, () => {
      const base = schema.examples[0]!;
      expect(() =>
        parseIntent(
          { schemaVersion: 1, requestId: 'req_1', intent: { ...base, approved: true } },
          'req_1',
        ),
      ).toThrow();
    });
  }

  it('archive schema lists exactly the contract enums', () => {
    const p = INTENT_SCHEMAS['archive.create'].properties as Record<string, { enum?: string[] }>;
    expect(p.format).toEqual({ enum: ['zip', '7z'] });
    expect(p.level).toEqual({ enum: ['store', 'fast', 'normal', 'maximum', 'ultra'] });
    expect(p.sourcePane!.enum).toEqual(['left', 'right', 'activeAtSubmit']);
  });
});

describe('offeredCapabilityIds() — what the provider is told it may use', () => {
  it('offers only ready+enabled capabilities and picks up a gate that opens later', () => {
    const d = defs();
    const r = new CapabilityRegistry(d, allActions);
    expect(r.offeredCapabilityIds()).toEqual(['archive.create']);
    d[1]!.engine = { status: 'ready' };
    expect(r.offeredCapabilityIds()).toEqual(['archive.create', 'directories.sync']);
  });

  it('drops a capability whose UI action is missing or disabled', () => {
    const r = new CapabilityRegistry(
      defs({ 'directories.sync': { engine: { status: 'ready' } } }),
      (id) => (id === 'file.pack' ? { run: () => {} } : undefined),
    );
    expect(r.offeredCapabilityIds()).toEqual(['archive.create']);
  });

  it('offers nothing when every capability is blocked', () => {
    const r = new CapabilityRegistry(
      defs({ 'archive.create': { engine: { status: 'pending' } } }),
      allActions,
    );
    expect(r.offeredCapabilityIds()).toEqual([]);
  });
});
