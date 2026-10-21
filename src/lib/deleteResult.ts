/** FER-585: wording for what a delete job did (dialog text, dock summary, retry list). */
import type { DeleteJobResult } from './ipc/deleteJobs';
import { baseName } from './stores/jobsStore.svelte';

function names(paths: string[], max = 3): string {
  const shown = paths.slice(0, max).map(baseName).join(', ');
  return paths.length > max ? `${shown} …` : shown;
}

/** True when the job did not finish cleanly: something failed, was left partly deleted or untouched. */
export function deleteHasProblems(r: DeleteJobResult): boolean {
  return (
    r.cancelled || r.partial.length > 0 || r.notProcessed.length > 0 || r.report.failed.length > 0
  );
}

/** Something on disk changed (a view or a scan is stale). */
export function deleteChangedDisk(r: DeleteJobResult): boolean {
  return r.report.deleted > 0 || r.partial.length > 0;
}

/** The paths that still exist because the job was cancelled: a retry continues with these. */
export function deleteRemaining(r: DeleteJobResult): string[] {
  return [...new Set([...r.partial, ...r.notProcessed])];
}

/** Dialog text for a job that did not end cleanly. Empty when everything was deleted. */
export function deleteProblemText(r: DeleteJobResult): string {
  const parts: string[] = [];
  const { requested, deleted, failed } = r.report;
  if (r.cancelled) parts.push(`Cancelled: ${deleted} of ${requested} deleted.`);
  // A folder that failed in the middle already says so in its failure text.
  const failedPaths = new Set(failed.map((f) => f.path));
  const partial = r.partial.filter((p) => !failedPaths.has(p));
  if (partial.length > 0) {
    const one = partial.length === 1;
    parts.push(
      `${one ? '1 folder was' : `${partial.length} folders were`} only partly deleted and ${one ? 'still exists' : 'still exist'}: ${names(partial)}.`,
    );
  }
  if (r.notProcessed.length > 0) {
    parts.push(
      `${r.notProcessed.length === 1 ? '1 item was' : `${r.notProcessed.length} items were`} not touched.`,
    );
  }
  if (failed.length > 0) {
    parts.push(
      `${failed.length} of ${requested} could not be deleted: ${failed.map((f) => f.message).join('; ')}`,
    );
  }
  return parts.join(' ');
}

/** One-line outcome for the dock / toast. */
export function deleteSummary(r: DeleteJobResult): string {
  const { requested, deleted, failed } = r.report;
  if (r.cancelled) {
    const partial = r.partial.length > 0 ? `, ${r.partial.length} folder partly deleted` : '';
    return `Delete cancelled: ${deleted}/${requested} deleted${partial}`;
  }
  if (failed.length > 0) return `Delete failed: ${failed.length} of ${requested} items`;
  return `Delete finished: ${deleted} items`;
}

export function deleteOutcome(r: DeleteJobResult): 'done' | 'failed' | 'canceled' {
  if (r.cancelled) return 'canceled';
  return r.report.failed.length > 0 ? 'failed' : 'done';
}
