import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import MediaPlayer from './MediaPlayer.svelte';
import { isPlayableAudioExt, isPlayableVideoExt } from '../preview/media';

vi.mock('../ipc/media', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  mediaGrantOne: async (path: string) =>
    `http://ferrum-media.localhost/tok-${encodeURIComponent(path)}`,
  mediaRevoke: vi.fn(async () => {}),
}));

const proto = HTMLMediaElement.prototype;
let play: ReturnType<typeof vi.fn>;
let pause: ReturnType<typeof vi.fn>;
let load: ReturnType<typeof vi.fn>;

beforeEach(() => {
  play = vi.fn(() => Promise.resolve());
  pause = vi.fn();
  load = vi.fn();
  vi.spyOn(proto, 'play').mockImplementation(play as never);
  vi.spyOn(proto, 'pause').mockImplementation(pause as never);
  vi.spyOn(proto, 'load').mockImplementation(load as never);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const settle = () => new Promise((r) => setTimeout(r, 0));
const base = { path: 'C:\\v\\a.mp4', name: 'a.mp4', kind: 'video' as const };

describe('MediaPlayer', () => {
  it('renders controls, metadata preload, asset src, aria-label, no <source>', async () => {
    const { container } = render(MediaPlayer, { ...base, autoplay: false });
    await tick();
    await settle();
    const v = container.querySelector('video')!;
    expect(v.hasAttribute('controls')).toBe(true);
    expect(v.getAttribute('preload')).toBe('metadata');
    expect(v.getAttribute('src')).toContain('http://ferrum-media.localhost/');
    expect(v.getAttribute('aria-label')).toContain('a.mp4');
    expect(container.querySelector('source')).toBeNull();
  });

  it('renders audio element for kind=audio', async () => {
    const { container } = render(MediaPlayer, {
      path: 'x.mp3',
      name: 'x.mp3',
      kind: 'audio',
      autoplay: false,
    });
    await tick();
    expect(container.querySelector('audio')).not.toBeNull();
  });

  it('autoplay off never calls play', async () => {
    render(MediaPlayer, { ...base, autoplay: false });
    await tick();
    expect(play).not.toHaveBeenCalled();
  });

  it('autoplay on plays muted', async () => {
    const { container } = render(MediaPlayer, { ...base, autoplay: true });
    await tick();
    expect(play).toHaveBeenCalledTimes(1);
    expect(container.querySelector('video')!.muted).toBe(true);
  });

  it('rejected play() shows neutral message, no error', async () => {
    play.mockImplementation(() => Promise.reject(new DOMException('no', 'NotAllowedError')));
    const { findByText, queryByRole } = render(MediaPlayer, { ...base, autoplay: true });
    expect(await findByText('Autoplay blocked — press play')).toBeTruthy();
    expect(queryByRole('alert')).toBeNull();
  });

  it('changing path releases old stream and loads the new one', async () => {
    const { container, rerender } = render(MediaPlayer, { ...base, autoplay: false });
    await tick();
    const v = container.querySelector('video')!;
    await rerender({ ...base, path: 'C:\\v\\b.mp4', name: 'b.mp4', autoplay: false });
    await tick();
    expect(pause).toHaveBeenCalled();
    expect(load).toHaveBeenCalled();
    expect(v.getAttribute('src')).toContain(encodeURIComponent('C:\\v\\b.mp4'));
  });

  it('toggling autoplay on does not restart the current file; new path does', async () => {
    const { rerender } = render(MediaPlayer, { ...base, autoplay: false });
    await tick();
    await rerender({ ...base, autoplay: true });
    await tick();
    expect(play).not.toHaveBeenCalled();
    await rerender({ ...base, path: 'n.mp4', name: 'n.mp4', autoplay: true });
    await tick();
    expect(play).toHaveBeenCalledTimes(1);
  });

  it('toggling autoplay off does not pause playing media', async () => {
    const { rerender } = render(MediaPlayer, { ...base, autoplay: true });
    await tick();
    pause.mockClear();
    await rerender({ ...base, autoplay: false });
    await tick();
    expect(pause).not.toHaveBeenCalled();
  });

  it('destroy pauses and drops src', async () => {
    const { container, unmount } = render(MediaPlayer, { ...base, autoplay: true });
    await tick();
    const v = container.querySelector('video')!;
    unmount();
    expect(pause).toHaveBeenCalled();
    expect(load).toHaveBeenCalled();
    expect(v.hasAttribute('src')).toBe(false);
  });

  it('error event shows honest message', async () => {
    const { container, findByRole } = render(MediaPlayer, { ...base, autoplay: false });
    await tick();
    await fireEvent.error(container.querySelector('video')!);
    expect((await findByRole('alert')).textContent).toBe(
      'Cannot play this file (unsupported or corrupt format)',
    );
  });
});

describe('media ext helpers', () => {
  it('video', () => {
    for (const n of ['a.mp4', 'a.WEBM', 'a.mov', 'a.m4v', 'a.ogv', 'C:\\x\\y.Mp4'])
      expect(isPlayableVideoExt(n)).toBe(true);
    for (const n of ['a.mkv', 'a.avi', 'mp4', '.mp4', 'a.mp3', ''])
      expect(isPlayableVideoExt(n)).toBe(false);
  });
  it('audio', () => {
    for (const n of ['a.mp3', 'a.WAV', 'a.ogg', 'a.flac', 'a.m4a', 'a.aac'])
      expect(isPlayableAudioExt(n)).toBe(true);
    for (const n of ['a.mp4', 'a.wma', 'a']) expect(isPlayableAudioExt(n)).toBe(false);
  });
});
