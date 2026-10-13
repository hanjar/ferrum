// AI-06 / FER-197: port ke operation queue Ferrum. Executor hanya membaca/membatalkan job ID
// milik run-nya; adapter ke opsStore (`ops.all` + `ops.cancel`) dipasang integrator.
import type { OperationProgress } from '../ipc/types';

export interface QueuePort {
  get(jobId: string): OperationProgress | undefined;
  /** Notifikasi perubahan progres; mengembalikan fungsi unsubscribe. */
  subscribe(listener: (p: OperationProgress) => void): () => void;
  /** Kirim cancel ke queue. Status akhir tetap datang dari progres job. */
  cancel(jobId: string): Promise<void>;
}

export interface PollingSource {
  read(): readonly OperationProgress[];
  cancel(jobId: string): Promise<void>;
}

/**
 * Adapter queue berbasis snapshot (mis. `() => ops.all`). `tick()` membandingkan snapshot dan
 * memberi tahu pendengar hanya untuk job yang berubah; `start(ms)` menjalankannya berkala.
 */
export class PollingQueuePort implements QueuePort {
  private readonly listeners = new Set<(p: OperationProgress) => void>();
  private readonly seen = new Map<string, string>();
  private current = new Map<string, OperationProgress>();
  private timer: ReturnType<typeof setInterval> | null = null;

  constructor(private readonly source: PollingSource) {}

  get(jobId: string): OperationProgress | undefined {
    return this.current.get(jobId) ?? this.source.read().find((p) => p.operationId === jobId);
  }

  subscribe(listener: (p: OperationProgress) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  cancel(jobId: string): Promise<void> {
    return this.source.cancel(jobId);
  }

  tick(): void {
    const next = new Map<string, OperationProgress>();
    for (const p of this.source.read()) next.set(p.operationId, p);
    this.current = next;
    for (const [id, p] of next) {
      const sig = `${p.state}|${p.processedFiles}|${p.processedBytes}|${p.failures?.length ?? 0}`;
      if (this.seen.get(id) === sig) continue;
      this.seen.set(id, sig);
      for (const l of [...this.listeners]) l(p);
    }
  }

  start(intervalMs = 250): void {
    if (this.timer) return;
    this.timer = setInterval(() => this.tick(), intervalMs);
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }
}
