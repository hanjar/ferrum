/**
 * FER-467 R8: network paths. Opening a UNC / WebDAV folder (`\\host\share`, `//host/share`,
 * `\\host@SSL\DavWWWRoot`, `\\?\UNC\host\share`) makes Windows contact that host and, on SMB,
 * offer the user's NTLM credentials. The UI marks such paths ("network") wherever a click opens
 * them (hotlist menu, Command Palette, folder history) and the hotlist asks once per host the user
 * has not opened before.
 *
 * Pure string checks only: nothing here touches the filesystem or the network. A drive letter
 * mapped to a share is not recognisable from the string and is not marked (it was mapped by the
 * user on purpose).
 */

const KNOWN_KEY = 'ferrum.network.knownHosts.v1';
const MAX_KNOWN = 200;

/** Placeholder host for a GLOBALROOT path whose real target cannot be read from the string. */
export const UNKNOWN_HOST = 'globalroot';

const firstPart = (rest: string): string => rest.split('\\')[0].split('@')[0].trim().toLowerCase();

/**
 * Host of a network path, lower-cased, without the WebDAV `@SSL` / `@port` suffix; `null` for a
 * local path or a local device path (`\\.\pipe\x`, `\\?\C:\`).
 *
 * FER-467 H1: the device spellings that reach the network count too: `\\?\UNC\host`,
 * `\\.\UNC\host`, `\??\UNC\host`, anything containing `\Device\Mup\host` (the network redirector,
 * e.g. `\\?\GLOBALROOT\Device\Mup\host\share`), the other redirectors (`\Device\LanmanRedirector\`,
 * `\Device\WebDavRedirector\`), and any other `GLOBALROOT` path (host unknown: `globalroot`).
 */
export function networkHost(path: string): string | null {
  const p = path.trim().replace(/\//g, '\\');
  const redirector = /\\device\\(?:mup|lanmanredirector|webdavredirector)\\(?:;[^\\]*\\)?/i.exec(p);
  if (redirector) {
    return firstPart(p.slice(redirector.index + redirector[0].length)) || UNKNOWN_HOST;
  }
  const unc = /^(?:\\\\[?.]|\\\?\?)\\UNC\\/i.exec(p);
  if (unc) return firstPart(p.slice(unc[0].length)) || UNKNOWN_HOST;
  if (/^(?:\\\\[?.]|\\\?\?)\\GLOBALROOT(?:\\|$)/i.test(p)) return UNKNOWN_HOST;
  if (/^\\\\[?.](?:\\|$)/.test(p)) return null;
  if (!p.startsWith('\\\\')) return null;
  const host = firstPart(p.slice(2));
  return host === '' ? null : host;
}

export function isNetworkPath(path: string | null | undefined): boolean {
  return !!path && networkHost(path) !== null;
}

/** Distinct hosts among `paths`, in order. */
export function networkHosts(paths: readonly (string | null | undefined)[]): string[] {
  const out: string[] = [];
  for (const p of paths) {
    const h = p ? networkHost(p) : null;
    if (h && !out.includes(h)) out.push(h);
  }
  return out;
}

export interface HostMemory {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function defaultMemory(): HostMemory | null {
  try {
    if (typeof localStorage !== 'undefined') return localStorage;
  } catch {
    /* storage unavailable (privacy mode) */
  }
  return null;
}

/**
 * Hosts the user already agreed to contact. Kept per machine in this window's storage; when that
 * storage is unavailable the list lives only for this session (so the user may be asked again).
 */
export class KnownHosts {
  private session = new Set<string>();

  constructor(private memory: HostMemory | null = defaultMemory()) {}

  private read(): string[] {
    try {
      const raw = this.memory?.getItem(KNOWN_KEY);
      const list: unknown = raw ? JSON.parse(raw) : [];
      return Array.isArray(list) ? list.filter((h): h is string => typeof h === 'string') : [];
    } catch {
      return [];
    }
  }

  has(host: string): boolean {
    const h = host.toLowerCase();
    return this.session.has(h) || this.read().includes(h);
  }

  add(hosts: readonly string[]): void {
    const list = this.read();
    for (const host of hosts) {
      const h = host.toLowerCase();
      this.session.add(h);
      if (!list.includes(h)) list.push(h);
    }
    try {
      this.memory?.setItem(KNOWN_KEY, JSON.stringify(list.slice(-MAX_KNOWN)));
    } catch {
      /* the session set still remembers */
    }
  }

  /** The hosts in `hosts` that were never confirmed. */
  unknown(hosts: readonly string[]): string[] {
    return hosts.filter((h) => !this.has(h));
  }
}
