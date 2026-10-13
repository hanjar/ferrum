/**
 * Clipboard writes (F2-14). Prefers the async Clipboard API and falls back
 * to a hidden-textarea copy for environments without it. Returns success so
 * callers can surface a real error instead of failing silently.
 */
export async function copyText(text: string): Promise<boolean> {
  const clip = globalThis.navigator?.clipboard;
  if (clip && typeof clip.writeText === 'function') {
    try {
      await clip.writeText(text);
      return true;
    } catch {
      // Denied/failed — try the legacy path below.
    }
  }
  try {
    const doc = globalThis.document;
    if (!doc || typeof doc.execCommand !== 'function') return false;
    const ta = doc.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    doc.body.appendChild(ta);
    ta.select();
    const ok = doc.execCommand('copy');
    ta.remove();
    return ok;
  } catch {
    return false;
  }
}
