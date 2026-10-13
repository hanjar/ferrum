/** Display formatters — pure, unit-tested. */

/** "1.6 GB" / "86.2 KB" / "612 B" — 3 significant digits, base 1024. */
export function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return bytes === 0 ? '0 B' : '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  let v = bytes;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  const digits = v >= 100 ? 0 : v >= 10 ? 1 : 2;
  return `${Number(v.toPrecision(3)).toFixed(digits)} ${units[u]}`;
}

/** "2026-09-25 16:20" in LOCAL time (prototype format), '' for unknown(0). */
export function formatDate(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '';
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/**
 * FER-495 Ctrl+\: the root of the drive or UNC share a path lives on (also for paths inside an
 * archive, which are physical `X:\dir\a.zip\inner` paths). `''` when the path has no root.
 */
export function rootOf(path: string): string {
  const p = path.replace(/\//g, '\\');
  const drive = /^(\\\\\?\\)?([A-Za-z]:)/.exec(p);
  if (drive) return `${drive[1] ?? ''}${drive[2]}\\`;
  const unc = /^\\\\([^\\]+)\\([^\\]+)/.exec(p);
  if (unc) return `\\\\${unc[1]}\\${unc[2]}\\`;
  return p.startsWith('\\') ? '\\' : '';
}

/** Parent path of a listing entry (".." row target). */
export function parentOf(path: string): string {
  const norm = path.replace(/[\\/]+$/, '');
  const i = Math.max(norm.lastIndexOf('/'), norm.lastIndexOf('\\'));
  if (i === -1) return '';
  if (i === 0) return norm.length > 1 ? '/' : '';
  return norm.slice(0, i + 1); // keep trailing separator for roots like "C:\"
}
