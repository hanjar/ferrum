// FER-486: fake attachment port for ChatStore/ChatPanel tests.
import {
  AskError,
  type AskInput,
  type AskOutcome,
  type AttachmentAsker,
  type AttachmentDestination,
  type AttachmentInfo,
  type InspectResult,
} from '../attachments';

export class FakeAsker implements AttachmentAsker {
  asks: AskInput[] = [];
  inspected: string[][] = [];
  infos = new Map<string, AttachmentInfo>();
  reply: AskOutcome | Error = { answer: 'Catatan rapat.', attachments: [] };
  pending: { resolve: (v: AskOutcome) => void; reject: (e: unknown) => void } | null = null;
  hold = false;
  dest: AttachmentDestination | undefined = { hostPort: '127.0.0.1:20128', local: true };
  inspectIds: (string | undefined)[] = [];
  cancelledInspections: string[] = [];
  async inspect(paths: string[], requestId?: string): Promise<InspectResult[]> {
    this.inspected.push(paths);
    this.inspectIds.push(requestId);
    return paths.map((path) => {
      const found = this.infos.get(path);
      return found ? { path, info: found } : { path, error: 'File not found' };
    });
  }
  ask(input: AskInput): Promise<AskOutcome> {
    this.asks.push(input);
    if (this.hold)
      return new Promise((resolve, reject) => {
        this.pending = { resolve, reject };
        input.signal.addEventListener('abort', () => reject(new AskError('cancelled')));
      });
    return this.reply instanceof Error ? Promise.reject(this.reply) : Promise.resolve(this.reply);
  }
  destination() {
    return this.dest;
  }
  cancelInspect(id: string) {
    this.cancelledInspections.push(id);
  }
  add(...items: AttachmentInfo[]) {
    for (const item of items) this.infos.set(item.path, item);
    return items.map((i) => ({ path: i.path, name: i.name, isDir: i.kind === 'folder' }));
  }
}
