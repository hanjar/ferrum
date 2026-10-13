/** FER-464 — how old a cached Disk usage scan is, in words, and whether it
 *  deserves a warning. Pure so the rules are unit-tested. */

const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

/** "baru saja" / "5 menit lalu" / "3 jam lalu" / "2 hari lalu". */
export function ageLabel(scannedAtMs: number, nowMs: number): string {
  if (!Number.isFinite(scannedAtMs) || scannedAtMs <= 0) return 'waktu tidak diketahui';
  const age = Math.max(0, nowMs - scannedAtMs);
  if (age < MIN) return 'baru saja';
  if (age < HOUR) return `${Math.floor(age / MIN)} menit lalu`;
  if (age < DAY) return `${Math.floor(age / HOUR)} jam lalu`;
  return `${Math.floor(age / DAY)} hari lalu`;
}

/** The warning line for a cached scan, or null when it is recent and no
 *  in-app change touched the root. A mutation made inside Ferrum after the
 *  scan always warns; otherwise the warning appears once the age passes
 *  `warnHours` (Settings, default 24). An unknown scan time warns too. */
export function cacheWarning(
  scannedAtMs: number,
  nowMs: number,
  warnHours: number,
  mutated: boolean,
): string | null {
  if (mutated) return 'Ada perubahan (hapus/ganti nama/pindah) lewat Ferrum sejak scan ini.';
  if (!Number.isFinite(scannedAtMs) || scannedAtMs <= 0) return 'Waktu scan tidak diketahui.';
  if (nowMs - scannedAtMs > warnHours * HOUR) {
    return `Hasil lebih tua dari ${warnHoursLabel(warnHours)} — angka bisa jauh berbeda dari kondisi sekarang.`;
  }
  return null;
}

function warnHoursLabel(h: number): string {
  return h % 24 === 0 ? `${h / 24} hari` : `${h} jam`;
}
