import { describe, expect, it, vi } from 'vitest';
import { chooseScanMode, type OfferDeps } from './fastScanOffer';

const ROOT = 'C:\\';

function deps(over: Partial<OfferDeps> = {}): OfferDeps {
  return {
    offerEnabled: () => true,
    eligibility: vi.fn(async () => ({ eligible: true, volume: 'C' })),
    ask: vi.fn(async () => ({ mode: 'fast' as const, dontAskAgain: false })),
    disableOffer: vi.fn(),
    ...over,
  };
}

describe('chooseScanMode (FER-460)', () => {
  it('offer disabled → regular scan without asking or probing', async () => {
    const d = deps({ offerEnabled: () => false });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('normal');
    expect(d.eligibility).not.toHaveBeenCalled();
    expect(d.ask).not.toHaveBeenCalled();
  });

  it('ineligible root (UNC, FAT, helper missing) → regular scan, no dialog', async () => {
    const d = deps({ eligibility: vi.fn(async () => ({ eligible: false, reason: 'FAT32' })) });
    await expect(chooseScanMode('E:\\', d)).resolves.toBe('normal');
    expect(d.ask).not.toHaveBeenCalled();
  });

  it('eligibility failure degrades to the regular scan', async () => {
    const d = deps({
      eligibility: vi.fn(async () => {
        throw new Error('ipc down');
      }),
    });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('normal');
  });

  it('eligible → the user choice decides', async () => {
    await expect(chooseScanMode(ROOT, deps())).resolves.toBe('fast');
    const normal = deps({
      ask: vi.fn(async () => ({ mode: 'normal' as const, dontAskAgain: false })),
    });
    await expect(chooseScanMode(ROOT, normal)).resolves.toBe('normal');
  });

  it('dismissed dialog starts nothing', async () => {
    const d = deps({ ask: vi.fn(async () => ({ mode: null, dontAskAgain: false })) });
    await expect(chooseScanMode(ROOT, d)).resolves.toBeNull();
  });

  it('"jangan tanya lagi" only switches the offer off — never remembers elevation', async () => {
    const d = deps({ ask: vi.fn(async () => ({ mode: 'fast' as const, dontAskAgain: true })) });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('fast');
    expect(d.disableOffer).toHaveBeenCalledTimes(1);
  });
});

describe('chooseScanMode — FER-476 "Instan (indeks)"', () => {
  it('index ready but no MFT helper → the offer still appears with the index option', async () => {
    const ask = vi.fn(async () => ({ mode: 'index' as const, dontAskAgain: false }));
    const d = deps({
      eligibility: vi.fn(async () => ({ eligible: false, reason: 'helper missing' })),
      indexEligibility: vi.fn(async () => ({ eligible: true, volume: 'C' })),
      ask,
    });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('index');
    expect(ask).toHaveBeenCalledWith(
      expect.objectContaining({ eligible: false }),
      expect.objectContaining({ eligible: true }),
    );
  });

  it('a mode the backend cannot serve is never returned', async () => {
    const d = deps({
      indexEligibility: vi.fn(async () => ({ eligible: false })),
      ask: vi.fn(async () => ({ mode: 'index' as const, dontAskAgain: false })),
    });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('normal');
  });

  it('index probe failure is just "not offered"', async () => {
    const ask = vi.fn(async () => ({ mode: 'fast' as const, dontAskAgain: false }));
    const d = deps({
      indexEligibility: vi.fn(async () => {
        throw new Error('pipe');
      }),
      ask,
    });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('fast');
    expect(ask).toHaveBeenCalledWith(expect.objectContaining({ eligible: true }), undefined);
  });

  it('offer disabled → regular scan, the index is not chosen silently', async () => {
    const d = deps({
      offerEnabled: () => false,
      indexEligibility: vi.fn(async () => ({ eligible: true, volume: 'C' })),
    });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('normal');
  });
});

describe('chooseScanMode instantDefault (FER-464, Disk usage)', () => {
  const idx = { eligible: true, volume: 'C' };

  it('index-covered root → "index" at once: no question, MFT never probed', async () => {
    const d = deps({ instantDefault: true, indexEligibility: vi.fn(async () => idx) });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('index');
    expect(d.ask).not.toHaveBeenCalled();
    expect(d.eligibility).not.toHaveBeenCalled();
  });

  it('"Jangan tanya lagi" only silences the offer, so the instant default still applies', async () => {
    const d = deps({
      instantDefault: true,
      offerEnabled: () => false,
      indexEligibility: vi.fn(async () => idx),
    });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('index');
    expect(d.disableOffer).not.toHaveBeenCalled();
  });

  it('root outside the index keeps the old rules (offer, or regular when switched off)', async () => {
    const no = vi.fn(async () => ({ eligible: false, reason: 'tidak diindeks' }));
    const on = deps({ instantDefault: true, indexEligibility: no });
    await expect(chooseScanMode(ROOT, on)).resolves.toBe('fast');
    expect(on.ask).toHaveBeenCalledTimes(1);
    const off = deps({ instantDefault: true, indexEligibility: no, offerEnabled: () => false });
    await expect(chooseScanMode(ROOT, off)).resolves.toBe('normal');
    expect(off.ask).not.toHaveBeenCalled();
  });

  it('index probe failure → falls back to the usual flow', async () => {
    const d = deps({
      instantDefault: true,
      indexEligibility: vi.fn(async () => {
        throw new Error('pipe down');
      }),
    });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('fast');
  });

  it('without the flag (Sync) the index is still only a button of the offer', async () => {
    const d = deps({
      indexEligibility: vi.fn(async () => idx),
      ask: vi.fn(async () => ({ mode: 'normal' as const, dontAskAgain: false })),
    });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('normal');
    expect(d.ask).toHaveBeenCalledTimes(1);
  });

  it('manual "Pilihan scan" shows the offer even when switched off and never changes the preference', async () => {
    const d = deps({
      instantDefault: true,
      forceOffer: true,
      offerEnabled: () => false,
      indexEligibility: vi.fn(async () => idx),
      ask: vi.fn(async () => ({ mode: 'normal' as const, dontAskAgain: true })),
    });
    await expect(chooseScanMode(ROOT, d)).resolves.toBe('normal');
    expect(d.ask).toHaveBeenCalledTimes(1);
    expect(d.disableOffer).not.toHaveBeenCalled();
  });
});
