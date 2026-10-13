import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  batchUnsafeArgs,
  commandLine,
  commandProblem,
  commandRisk,
  needsCmdSafeArgs,
  parseDoc,
  quoteArg,
} from './model';

const here = dirname(fileURLToPath(import.meta.url));
const fixture = (name: string): string =>
  readFileSync(join(here, '../../../contract-fixtures/hotlist', name), 'utf-8');

const cases = JSON.parse(fixture('command-cases.json')) as {
  rejectCommands: string[];
  acceptCommands: string[];
  batchArgs: { command: string; arg: string; unsafe: boolean }[];
};

describe('format characters (bidi and friends)', () => {
  const doc = (item: unknown) => parseDoc({ version: 2, items: [item] });
  const evil = ['\u202E', '\u202A', '\u202D', '\u2066', '\u2069', '\u200E', '\u200F', '\u061C'];

  it('rejects them in titles, paths, commands, arguments and the source note', () => {
    for (const c of [...evil, '\u200B', '\uFEFF', '\u2060', '\u00AD', '\u{E0041}']) {
      const bad = `C:\\inv\\${c}fdp.exe`;
      expect(doc({ type: 'dir', title: bad, path: 'C:\\a' }), c).toBeNull();
      expect(doc({ type: 'dir', title: 'a', path: bad }), c).toBeNull();
      expect(doc({ type: 'dir', title: 'a', path: 'C:\\a', targetPath: bad }), c).toBeNull();
      expect(doc({ type: 'submenu', title: bad }), c).toBeNull();
      expect(doc({ type: 'command', title: 'c', command: bad, args: [] }), c).toBeNull();
      expect(doc({ type: 'command', title: 'c', command: 'C:\\a.exe', args: [bad] }), c).toBeNull();
      expect(parseDoc({ version: 2, items: [], source: `n${c}` }), c).toBeNull();
    }
  });

  it('keeps ZWNJ/ZWJ, which some scripts need', () => {
    expect(doc({ type: 'dir', title: 'می\u200Cخواهم', path: 'C:\\a' })).not.toBeNull();
  });
});

describe('command items: what may run', () => {
  const cmd = (command: string, args: string[] = []) =>
    parseDoc({ version: 2, items: [{ type: 'command', title: 'c', command, args }] });

  it('batch files refuse cmd metacharacters in arguments (BatBadBut)', () => {
    for (const command of ['C:\\s\\run.bat', 'C:\\s\\RUN.CMD', 'C:\\s\\run.cmd.', 'tool']) {
      for (const arg of ['x" & calc & "', 'a&b', 'a|b', 'a<b', 'a>b', 'a^b', '%PATH%', 'q"r']) {
        expect(cmd(command, [arg]), `${command} ${arg}`).toBeNull();
        expect(batchUnsafeArgs(command, [arg])).toBe(true);
      }
      expect(cmd(command, ['plain', 'two words'])).not.toBeNull();
    }
    // Real executables take these characters as plain data; everything else gets the strict check.
    for (const command of ['C:\\t\\a.exe', 'C:\\t\\a.com']) {
      expect(cmd(command, ['a&b', '%x%'])).not.toBeNull();
    }
    for (const command of ['C:\\a\\go.ps1', 'C:\\a\\link.lnk', 'C:\\a\\tool.v2']) {
      expect(cmd(command, ['a&b']), command).toBeNull();
    }
    expect(needsCmdSafeArgs('C:\\dir.bat\\tool.exe')).toBe(false);
    expect(needsCmdSafeArgs('C:\\dir.with.dots\\tool')).toBe(true);
  });

  it('argument metacharacters follow the shared vectors (same file as the Rust tests)', () => {
    for (const c of cases.batchArgs) {
      expect(batchUnsafeArgs(c.command, [c.arg]), `${c.command} ${c.arg}`).toBe(c.unsafe);
    }
  });

  it('commands must be local programs: shared reject and accept vectors', () => {
    expect(cases.rejectCommands.length).toBeGreaterThanOrEqual(20);
    for (const bad of cases.rejectCommands) expect(cmd(bad), bad).toBeNull();
    for (const ok of cases.acceptCommands) expect(cmd(ok), ok).not.toBeNull();
    // Folders may live on the network.
    expect(
      parseDoc({ version: 2, items: [{ type: 'dir', title: 'n', path: '\\\\nas\\media' }] }),
    ).not.toBeNull();
  });

  it('explains why a command is refused', () => {
    expect(commandProblem('https://x.y', [])).toBe('uri');
    expect(commandProblem('\\\\h\\s\\x.exe', [])).toBe('network');
    expect(commandProblem('', [])).toBe('program');
    expect(commandProblem('C:\\r.bat', ['a&b'])).toBe('args_unsafe');
    expect(commandProblem('C:\\r.exe', ['a&b'])).toBeNull();
  });

  it('flags scripts and shortcuts for a warning', () => {
    for (const s of [
      'x.ps1',
      'x.VBS',
      'x.js',
      'x.hta',
      'x.scr',
      'x.bat',
      'x.cmd',
      'x.msc',
      'x.cmd.',
    ]) {
      expect(commandRisk(`C:\\d\\${s}`), s).toBe('script');
    }
    expect(commandRisk('C:\\d\\a.lnk')).toBe('shortcut');
    expect(commandRisk('C:\\d\\a.URL')).toBe('shortcut');
    expect(commandRisk('C:\\d\\a.exe')).toBeNull();
  });
});

describe('command line shown in the confirmation (same vectors as the Rust quote_arg)', () => {
  const vectors = JSON.parse(fixture('quote-arg.json')) as { arg: string; quoted: string }[];

  it('quoteArg matches every shared vector', () => {
    expect(vectors.length).toBeGreaterThanOrEqual(9);
    for (const v of vectors) expect(quoteArg(v.arg), JSON.stringify(v.arg)).toBe(v.quoted);
  });

  it('shows arguments the way the program receives them, so ["a b"] differs from ["a","b"]', () => {
    expect(commandLine('C:\\a b\\x.exe', ['a b'])).toBe('"C:\\a b\\x.exe" "a b"');
    expect(commandLine('C:\\a b\\x.exe', ['a', 'b'])).toBe('"C:\\a b\\x.exe" a b');
    expect(commandLine('tool.exe', [])).toBe('tool.exe');
    expect(commandLine('tool.exe', ['x" & calc & "'])).toBe('tool.exe "x\\" & calc & \\""');
  });
});
