// FER-486: chip lampiran di composer chat.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPanel from './ChatPanel.svelte';
import { ChatStore } from '../../ai/chatStore.svelte';
import { FakeExecutor } from '../../ai/fixtures/chatFixtures';
import { FakeAsker } from '../../ai/fixtures/attachmentFixtures';
import { resetDisclosuresForTests, type AttachmentInfo } from '../../ai/attachments';

beforeEach(() => resetDisclosuresForTests());
afterEach(cleanup);

const info = (name: string, extra: Partial<AttachmentInfo> = {}): AttachmentInfo => ({
  path: `C:\\d\\${name}`,
  name,
  kind: 'text',
  size: 2048,
  modifiedMs: 0,
  inArchive: false,
  support: 'content',
  ...extra,
});

async function mount() {
  const ex = new FakeExecutor();
  const asker = new FakeAsker();
  const store = new ChatStore(ex, asker);
  render(ChatPanel, { store, onclose: () => undefined, paneHint: () => ({ leftSelected: 0 }) });
  return { ex, asker, store };
}

describe('ChatPanel attachments (FER-486)', () => {
  it('shows name, size and kind per chip, the destination, and removes a chip', async () => {
    const { asker, store } = await mount();
    await store.addAttachments(
      asker.add(
        info('a.txt'),
        info('photo.png', {
          kind: 'image',
          support: 'metadata',
          note: 'isi tidak didukung (metadata saja)',
        }),
        info('big.log', { size: 500 * 1024 }),
      ),
    );
    await tick();
    const chips = screen.getAllByTestId('attachment-chip');
    expect(chips).toHaveLength(3);
    expect(chips[0].textContent).toContain('a.txt');
    expect(chips[0].textContent).toContain('2.0 KB');
    expect(chips[0].textContent).toContain('teks');
    expect(chips[1].textContent).toContain('metadata saja');
    expect(chips[2].textContent).toContain('akan dipotong ke 100.0 KB');
    expect(screen.getByTestId('attach-destination').textContent).toContain(
      'Isi 2 file (+ metadata 1 item) akan dikirim ke 127.0.0.1:20128',
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Hapus lampiran a.txt' }));
    expect(screen.getAllByTestId('attachment-chip')).toHaveLength(2);
  });

  it('a sensitive chip shows a red warning and Send stays disabled until ticked', async () => {
    const { asker, store } = await mount();
    await store.addAttachments(asker.add(info('.env', { sensitive: 'name' })));
    store.draft = 'isinya apa?';
    await tick();
    expect(screen.getByRole('alert').textContent).toContain('rahasia');
    const sendBtn = screen.getByRole('button', { name: 'Send' }) as HTMLButtonElement;
    expect(sendBtn.disabled).toBe(true);
    await fireEvent.click(screen.getByTestId('attachment-confirm'));
    await tick();
    expect(sendBtn.disabled).toBe(false);
  });

  it('renders the answer as text with what was sent', async () => {
    const { asker, store } = await mount();
    asker.reply = {
      answer: 'Ini **catatan** rapat.',
      attachments: [
        {
          path: 'C:\\d\\a.txt',
          name: 'a.txt',
          kind: 'text',
          support: 'content',
          size: 250000,
          sentBytes: 102400,
          truncated: true,
          note: 'dipotong: 102400 dari 250000 byte',
        },
      ],
    };
    await store.addAttachments(asker.add(info('a.txt')));
    store.send('file ini tentang apa?');
    await new Promise((r) => setTimeout(r, 0));
    await tick();
    const answer = screen.getByTestId('chat-answer');
    expect(answer.textContent).toContain('catatan');
    expect(screen.getByTestId('sent-notes').textContent).toContain(
      'a.txt: 100.0 KB isi dikirim (dipotong: 102400 dari 250000 byte)',
    );
    expect(screen.getByTestId('message-attachments').textContent).toContain('a.txt');
  });

  it('a remote provider shows the one-time disclosure first', async () => {
    const { asker, store } = await mount();
    asker.dest = { hostPort: 'api.z.ai', local: false };
    await store.addAttachments(asker.add(info('a.txt')));
    await tick();
    expect(screen.getByTestId('attach-disclosure').textContent).toContain('keluar dari PC ini');
    await fireEvent.click(screen.getByRole('button', { name: 'Mengerti' }));
    expect(screen.queryByTestId('attach-disclosure')).toBeNull();
  });
});
