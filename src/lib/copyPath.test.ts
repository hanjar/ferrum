import { describe, expect, it } from 'vitest';
import {
  displayPath,
  folderCopyText,
  folderPathText,
  itemPathText,
  namesText,
  parentFolder,
  pathsText,
  pickCopyEntries,
} from './copyPath';

const e = (path: string) => ({ name: path.split('\\').pop() ?? path, path });

describe('folderPathText (Ctrl+1)', () => {
  it('adds one trailing backslash', () => {
    expect(folderPathText('C:\\Users\\you\\projects')).toBe('C:\\Users\\you\\projects\\');
    expect(folderPathText('C:\\Users\\you\\projects\\')).toBe('C:\\Users\\you\\projects\\');
  });
  it('keeps a drive root as C:\\', () => {
    expect(folderPathText('C:\\')).toBe('C:\\');
    expect(folderPathText('C:')).toBe('C:\\');
  });
  it('shows UNC and long paths like the address bar', () => {
    expect(folderPathText('\\\\srv\\share\\dir')).toBe('\\\\srv\\share\\dir\\');
    expect(folderPathText('\\\\?\\UNC\\srv\\share\\dir')).toBe('\\\\srv\\share\\dir\\');
    expect(folderPathText('\\\\?\\C:\\very\\long')).toBe('C:\\very\\long\\');
  });
  it('treats an archive location as a folder inside the zip', () => {
    expect(folderPathText('C:\\data\\file.zip\\folder')).toBe('C:\\data\\file.zip\\folder\\');
    expect(folderPathText('C:\\data\\file.zip/folder')).toBe('C:\\data\\file.zip\\folder\\');
  });
  it('is empty for an empty path', () => {
    expect(folderPathText('')).toBe('');
  });
});

describe('itemPathText (Ctrl+3)', () => {
  it('has no trailing backslash, even for folders', () => {
    expect(itemPathText('C:\\a\\b\\')).toBe('C:\\a\\b');
    expect(itemPathText('C:\\a\\b.txt')).toBe('C:\\a\\b.txt');
  });
  it('keeps a bare drive root usable', () => {
    expect(itemPathText('C:\\')).toBe('C:\\');
  });
  it('strips the long-path prefix and handles archive members', () => {
    expect(itemPathText('\\\\?\\C:\\a\\b.txt')).toBe('C:\\a\\b.txt');
    expect(itemPathText('\\\\?\\UNC\\srv\\share\\b.txt')).toBe('\\\\srv\\share\\b.txt');
    expect(itemPathText('C:\\x\\f.zip\\dir\\item.txt')).toBe('C:\\x\\f.zip\\dir\\item.txt');
  });
});

describe('multi item text', () => {
  const items = [e('C:\\d\\one.txt'), e('C:\\d\\two.txt'), e('C:\\d\\three')];
  it('joins names with CRLF and no trailing newline', () => {
    expect(namesText(items)).toBe('one.txt\r\ntwo.txt\r\nthree');
  });
  it('joins paths with CRLF and no trailing newline', () => {
    expect(pathsText(items)).toBe('C:\\d\\one.txt\r\nC:\\d\\two.txt\r\nC:\\d\\three');
  });
  it('a single item has no separator', () => {
    expect(namesText([items[0]])).toBe('one.txt');
  });
});

describe('pickCopyEntries', () => {
  const visible = [e('C:\\d\\a'), e('C:\\d\\b'), e('C:\\d\\c')];
  it('uses the selection in visible order, not selection order', () => {
    const got = pickCopyEntries(visible, ['C:\\d\\c', 'C:\\d\\a'], e('C:\\d\\b'));
    expect(got.map((x) => x.name)).toEqual(['a', 'c']);
  });
  it('falls back to the cursor without a selection', () => {
    expect(pickCopyEntries(visible, [], visible[1])).toEqual([visible[1]]);
  });
  it('falls back to the cursor when no selected row is visible', () => {
    expect(pickCopyEntries(visible, ['C:\\gone'], visible[1])).toEqual([visible[1]]);
  });
  it('is empty with neither', () => {
    expect(pickCopyEntries(visible, [], undefined)).toEqual([]);
  });
});

describe('folderCopyText', () => {
  it('uses the pane folder', () => {
    expect(folderCopyText('C:\\x', e('C:\\y\\z.txt'))).toBe('C:\\x\\');
  });
  it('uses the cursor item folder when the pane has none', () => {
    expect(folderCopyText('', e('C:\\y\\z.txt'))).toBe('C:\\y\\');
    expect(folderCopyText('', e('C:\\z.txt'))).toBe('C:\\');
  });
  it('is empty with nothing to go on', () => {
    expect(folderCopyText('', undefined)).toBe('');
  });
});

describe('helpers', () => {
  it('displayPath normalises separators', () => {
    expect(displayPath('C:/a/b')).toBe('C:\\a\\b');
  });
  it('parentFolder', () => {
    expect(parentFolder('C:\\a\\b.txt')).toBe('C:\\a');
    expect(parentFolder('C:\\b.txt')).toBe('C:\\');
  });
});
