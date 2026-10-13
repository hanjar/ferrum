import { describe, expect, it } from 'vitest';
import { isNetworkPath, KnownHosts, networkHost, networkHosts } from './networkPath';

describe('networkHost (FER-467 R8)', () => {
  it('finds the host of every UNC / WebDAV spelling', () => {
    expect(networkHost('\\\\FileSrv\\team\\x')).toBe('filesrv');
    expect(networkHost('//filesrv/team')).toBe('filesrv');
    expect(networkHost('\\/filesrv/team')).toBe('filesrv');
    expect(networkHost('\\\\dav.example@SSL\\DavWWWRoot\\x')).toBe('dav.example');
    expect(networkHost('\\\\dav.example@8080\\x')).toBe('dav.example');
    expect(networkHost('\\\\?\\UNC\\srv\\share')).toBe('srv');
    expect(networkHost('  \\\\srv\\share  ')).toBe('srv');
  });

  it('local and device paths are not network paths', () => {
    for (const p of [
      'C:\\a',
      'C:/a',
      '\\\\?\\C:\\a',
      '\\\\.\\pipe\\x',
      '\\\\',
      '\\\\?',
      'a\\b',
      '',
    ]) {
      expect(networkHost(p)).toBeNull();
      expect(isNetworkPath(p)).toBe(false);
    }
    expect(isNetworkPath(undefined)).toBe(false);
    expect(isNetworkPath('\\\\srv\\s')).toBe(true);
  });

  it('device spellings that reach the network are network paths, with their host (H1)', () => {
    const cases: [string, string][] = [
      ['\\\\.\\UNC\\Evil\\share', 'evil'],
      ['//./unc/evil/share', 'evil'],
      ['\\??\\UNC\\evil\\share', 'evil'],
      ['\\\\?\\GLOBALROOT\\Device\\Mup\\evil\\share', 'evil'],
      ['\\\\.\\GLOBALROOT\\Device\\Mup\\evil\\share', 'evil'],
      ['//?/globalroot/device/mup/evil/share', 'evil'],
      ['\\Device\\Mup\\evil\\share', 'evil'],
      ['C:\\x\\..\\..\\Device\\Mup\\evil@SSL\\x', 'evil'],
      ['\\Device\\LanmanRedirector\\;Z:0000000000012345\\evil\\share', 'evil'],
      ['\\Device\\WebDavRedirector\\evil\\DavWWWRoot', 'evil'],
      ['\\\\?\\GLOBALROOT\\Device\\HarddiskVolume2\\x', 'globalroot'],
      ['\\\\.\\GLOBALROOT', 'globalroot'],
    ];
    for (const [p, host] of cases) {
      expect(networkHost(p), p).toBe(host);
      expect(isNetworkPath(p), p).toBe(true);
    }
  });

  it('collects distinct hosts', () => {
    expect(networkHosts(['\\\\a\\x', 'C:\\y', undefined, '//A/z', '\\\\b\\q'])).toEqual(['a', 'b']);
  });
});

describe('KnownHosts', () => {
  it('remembers confirmed hosts in storage, case-insensitively', () => {
    const mem = new Map<string, string>();
    const memory = {
      getItem: (k: string) => mem.get(k) ?? null,
      setItem: (k: string, v: string) => void mem.set(k, v),
    };
    const known = new KnownHosts(memory);
    expect(known.unknown(['a', 'b'])).toEqual(['a', 'b']);
    known.add(['A']);
    expect(known.has('a')).toBe(true);
    expect(new KnownHosts(memory).unknown(['a', 'b'])).toEqual(['b']);
  });

  it('works without storage (session only) and survives garbage', () => {
    const known = new KnownHosts(null);
    known.add(['x']);
    expect(known.has('x')).toBe(true);
    const broken = new KnownHosts({
      getItem: () => '{not json',
      setItem: () => {
        throw new Error('quota');
      },
    });
    expect(broken.has('x')).toBe(false);
    broken.add(['x']);
    expect(broken.has('x')).toBe(true);
  });
});
