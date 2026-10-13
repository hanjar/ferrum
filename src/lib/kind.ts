import type { FileEntry } from './ipc/types';

/** Icon/accent category for a row, mirroring the prototype's vivid set. */
export type Kind = 'dir' | 'code' | 'image' | 'media' | 'archive' | 'data' | 'docs' | 'text';

const EXT: Record<string, Kind> = {
  // code
  rs: 'code',
  ts: 'code',
  tsx: 'code',
  js: 'code',
  jsx: 'code',
  mjs: 'code',
  c: 'code',
  h: 'code',
  cpp: 'code',
  hpp: 'code',
  py: 'code',
  go: 'code',
  java: 'code',
  rb: 'code',
  php: 'code',
  sh: 'code',
  ps1: 'code',
  bat: 'code',
  html: 'code',
  css: 'code',
  scss: 'code',
  vue: 'code',
  svelte: 'code',
  // image
  png: 'image',
  jpg: 'image',
  jpeg: 'image',
  gif: 'image',
  webp: 'image',
  svg: 'image',
  bmp: 'image',
  ico: 'image',
  tiff: 'image',
  exr: 'image',
  // media
  mp4: 'media',
  mkv: 'media',
  mov: 'media',
  avi: 'media',
  webm: 'media',
  mp3: 'media',
  flac: 'media',
  wav: 'media',
  ogg: 'media',
  m4a: 'media',
  // archive
  zip: 'archive',
  rar: 'archive',
  '7z': 'archive',
  gz: 'archive',
  bz2: 'archive',
  xz: 'archive',
  tar: 'archive',
  dmg: 'archive',
  iso: 'archive',
  // data
  json: 'data',
  jsonl: 'data',
  csv: 'data',
  tsv: 'data',
  toml: 'data',
  yaml: 'data',
  yml: 'data',
  xml: 'data',
  sql: 'data',
  parquet: 'data',
  db: 'data',
  sqlite: 'data',
  // docs
  md: 'docs',
  txt: 'docs',
  pdf: 'docs',
  doc: 'docs',
  docx: 'docs',
  xls: 'docs',
  xlsx: 'docs',
  ppt: 'docs',
  pptx: 'docs',
  rtf: 'docs',
  log: 'text',
};

export function kindOf(entry: Pick<FileEntry, 'isDir' | 'name'>): Kind {
  if (entry.isDir) return 'dir';
  const dot = entry.name.lastIndexOf('.');
  if (dot <= 0 || dot === entry.name.length - 1) return 'text';
  return EXT[entry.name.slice(dot + 1).toLowerCase()] ?? 'text';
}
