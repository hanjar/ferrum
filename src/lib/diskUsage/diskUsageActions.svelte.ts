/**
 * DSK-06.1 + 06.2 — row/context actions over the Disk usage snapshot.
 *
 * Every action rides an EXISTING surface — there is no second implementation
 * of any mutation here:
 * - Reveal      → the pane's existing onreveal callback (parent navigation +
 *                 selection by the pane itself; never opens/executes).
 * - Properties  → the existing properties dialog (`entry_properties` IPC)
 *                 via propertiesStore.
 * - Copy path   → the existing clipboard helper.
 * - Move        → dialogs.beginTransferTo('move', …) — the same flow as
 *                 cross-pane drag: planCopy → confirm dialog → the existing
 *                 BTM queue (enqueueMove).
 * - Delete      → dialogs.beginDelete — the ONE existing confirm dialog →
 *                 the existing delete_entries call. No auto-clean/cleanup
 *                 action exists anywhere in this module (contract §2.3/§2.5;
 *                 size/ranking is never a deletion authorization).
 *
 * DSK-06.2 revalidation: a snapshot row is only a best-effort observation.
 * BEFORE Move/Delete open their existing flows, the target is re-statted
 * through the light existing `entry_properties` command and compared with
 * the snapshot (existence, kind, and — for files — logical size; the
 * snapshot carries no mtime baseline, so size+kind is the identity check we
 * honestly have). A missing/changed/verifiable target is REFUSED with a
 * "rescan" gate — a stale snapshot row is never handed to move/delete.
 */
import { entryProperties } from '../ipc/properties';
import { FsError, type DiskUsageEntryKind } from '../ipc/types';
import { copyText } from '../clipboard';
import { dialogs } from '../stores/dialogStore.svelte';
import { propertiesStore } from '../stores/propertiesStore.svelte';
import { inactivePane, paneById } from '../stores/panesRegistry.svelte';
import { duWorkspace } from './duWorkspace.svelte';
import type { DiskUsageStore } from './diskUsageStore.svelte';
import type { ContextMenuItem } from '../stores/contextMenuStore.svelte';

/** One snapshot row reduced to what revalidation can check. */
export interface DuScanTarget {
  path: string;
  kind: DiskUsageEntryKind;
  logicalBytes: number;
}

/** DSK-06.4 — the standing size honesty note (tooltip/info near metrics and
 *  on the scan-row Properties affordance). Verbatim contract stance: */
export const DU_SIZE_NOTE =
  'Ukuran logis ≠ ruang yang bisa dibebaskan — hardlink/kompresi/recycle bin ' +
  'membuat hasilnya berbeda; free space adalah metrik drive, bukan pengurangan ' +
  'hasil scan.';

/** Light re-stat through the EXISTING `entry_properties` command; returns a
 *  refusal reason or null when the target still matches the snapshot. */
export async function revalidateScanTarget(t: DuScanTarget): Promise<string | null> {
  let props;
  try {
    props = await entryProperties(t.path);
  } catch (e) {
    if (e instanceof FsError && e.code === 'notFound') {
      return 'Item tidak ada lagi di disk — hasil scan usang.';
    }
    return `Item tidak dapat diverifikasi (${e instanceof Error ? e.message : String(e)}) — rescan sebelum aksi.`;
  }
  if (props.kind !== t.kind) {
    return `Jenis item berubah sejak scan (scan: ${t.kind}, disk: ${props.kind}).`;
  }
  // Dir size is always 0 in properties (subtree totals are scan-only), so
  // the size identity check applies to files only.
  if (t.kind === 'file' && props.size !== t.logicalBytes) {
    return `Ukuran item berubah sejak scan (${t.logicalBytes} → ${props.size} B).`;
  }
  return null;
}

/** DSK-11: the move destination is the folder of the Files pane OPPOSITE the
 *  pane the workspace was entered from (`duWorkspace.entryPane` — both
 *  FilePanes keep their pre-entry cwd while they are hidden); with no
 *  registered opposite pane the standard inactive-pane flow applies. */
function moveTargetDir(): string | null {
  const fallback = inactivePane();
  const pane = paneById(duWorkspace.entryPane === 0 ? 1 : 0) ?? fallback;
  const dir = pane?.path.trim();
  return dir ? dir : null;
}

/** DSK-06.1 — Move through the EXISTING transfer flow only. A refusal lands
 *  on THIS store's gate (per instance — isolated per store). */
export async function moveScanEntryToOtherPane(
  store: DiskUsageStore,
  t: DuScanTarget,
): Promise<boolean> {
  store.clearDuGate();
  const reason = await revalidateScanTarget(t);
  if (reason) {
    store.duGate = { path: t.path, message: reason };
    return false;
  }
  const target = moveTargetDir();
  if (!target) {
    store.duGate = { path: t.path, message: 'Tidak ada pane lain sebagai folder tujuan Move.' };
    return false;
  }
  void dialogs.beginTransferTo('move', [t.path], target);
  return true;
}

/** DSK-06.1 — Delete through the ONE existing confirm dialog (trash default;
 *  the dialog's own permanent checkbox stays user-editable as elsewhere). */
export async function deleteScanEntryWithConfirm(
  store: DiskUsageStore,
  t: DuScanTarget,
): Promise<boolean> {
  store.clearDuGate();
  const reason = await revalidateScanTarget(t);
  if (reason) {
    store.duGate = { path: t.path, message: reason };
    return false;
  }
  dialogs.beginDelete(false, [t.path]);
  return true;
}

/** Existing properties dialog (entry_properties) — read-only, no revalidate
 *  needed: opening it on a vanished path shows the dialog's own error. */
export function openPropertiesFor(path: string): void {
  propertiesStore.open({ kind: 'properties', path });
}

/** Existing clipboard helper (same one the Files rows already use). */
export function copyScanPath(path: string): Promise<boolean> {
  return copyText(path);
}

/** Context-menu items for a snapshot row (Disk usage tree). `root` rows get
 *  the read-only set only — the scan root itself is never offered for
 *  Move/Delete from its own row. Still no cleanup item of any kind. */
export function duRowActionItems(
  store: DiskUsageStore,
  t: DuScanTarget,
  opts: { root: boolean; onreveal?: (filePath: string) => void },
): ContextMenuItem[] {
  const items: ContextMenuItem[] = [
    { label: 'Reveal in pane', disabled: !opts.onreveal, run: () => opts.onreveal?.(t.path) },
    { label: 'Properties', run: () => openPropertiesFor(t.path) },
    { label: 'Copy path', run: () => void copyScanPath(t.path) },
  ];
  if (!opts.root) {
    items.push({ separator: true });
    items.push({
      label: 'Move to other pane…',
      run: () => void moveScanEntryToOtherPane(store, t),
    });
    items.push({
      label: 'Delete…',
      danger: true,
      run: () => void deleteScanEntryWithConfirm(store, t),
    });
  }
  return items;
}
