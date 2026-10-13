// Fixture UI untuk AI-07: plan valid menurut kontrak AI-01 + FakeExecutor.
// FakeExecutor BUKAN executor nyata: ia hanya merelay update yang diperintahkan tes/dev.
import type { ApproveOptions, ChatExecutor, PlanItemRow, RunUpdate } from '../chatStore.svelte';
import { parseResolvedPlan, type ResolvedPlan } from '../contracts';

export function samplePlan(overrides: Partial<ResolvedPlan> = {}): ResolvedPlan {
  const pane = (id: 'left' | 'right', tab: string, path: string) => ({
    paneId: id,
    tabId: tab,
    location: { kind: 'disk' as const, path },
    contextRevision: 1,
    selectionRevision: 1,
    selectionHandle: `sel-${id}`,
    filter: '',
    showHidden: false,
    sort: { key: 'name' as const, dir: 1 as const },
  });
  return parseResolvedPlan({
    schemaVersion: 1,
    planId: 'plan-1',
    requestId: 'req-1',
    revision: 1,
    capability: 'archive.create',
    sourceSnapshot: {
      schemaVersion: 1,
      requestId: 'req-1',
      activePaneAtSubmit: 'left',
      panes: [pane('left', 'tab-l', 'D:\\Projects\\Invoice'), pane('right', 'tab-r', 'E:\\Backup')],
    },
    operation: {
      kind: 'archive.create',
      sourceManifest: 'man-1',
      targetDirectory: 'D:\\Projects\\Invoice',
      outputName: 'Invoice.zip',
    },
    preview: { itemCount: 14, conflictCount: 0, totalBytes: 4096 },
    steps: [{ id: 's1', capability: 'archive.create', dependsOn: [] }],
    previewDigest: 'digest-1',
    risk: 'mutating',
    requiresApproval: true,
    stepCount: 1,
    ...overrides,
  });
}

export function manyRows(
  total: number,
  offset: number,
  limit: number,
): { rows: PlanItemRow[]; total: number } {
  const rows: PlanItemRow[] = [];
  for (let i = offset; i < Math.min(total, offset + limit); i++) {
    rows.push({ ref: `item-${i}`, label: `file-${i}.txt`, detail: 'file' });
  }
  return { rows, total };
}

export class FakeExecutor implements ChatExecutor {
  submitted: Array<{ requestId: string; text: string }> = [];
  approved: string[] = [];
  approveOptions: Array<ApproveOptions | undefined> = [];
  cancelled: string[] = [];
  edited: string[] = [];
  itemCalls: Array<[string, number, number]> = [];
  itemTotal = 0;
  private listeners = new Set<(u: RunUpdate) => void>();
  submit(req: { requestId: string; text: string }): void {
    this.submitted.push(req);
  }
  approve(runId: string, options?: ApproveOptions): void {
    this.approved.push(runId);
    this.approveOptions.push(options);
  }
  cancel(runId: string): void {
    this.cancelled.push(runId);
  }
  edit(runId: string): void {
    this.edited.push(runId);
  }
  async listItems(runId: string, offset: number, limit: number) {
    this.itemCalls.push([runId, offset, limit]);
    return manyRows(this.itemTotal, offset, limit);
  }
  subscribe(l: (u: RunUpdate) => void): () => void {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  }
  emit(u: RunUpdate): void {
    for (const l of this.listeners) l(u);
  }
}
