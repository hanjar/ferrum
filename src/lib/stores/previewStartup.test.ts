import { beforeEach, describe, expect, it } from 'vitest';
import { applyStartupPreview, resetStartupPreviewForTest } from './previewStartup';
import { ui } from '../uiState.svelte';

beforeEach(() => {
  resetStartupPreviewForTest();
  ui.quickViewOpen = false;
  ui.preview.openOnLaunch = false;
});

describe('applyStartupPreview', () => {
  it('stays closed when openOnLaunch is OFF (also the failed-load default)', () => {
    applyStartupPreview();
    expect(ui.quickViewOpen).toBe(false);
  });

  it('opens the panel when openOnLaunch is ON', () => {
    ui.preview.openOnLaunch = true;
    applyStartupPreview();
    expect(ui.quickViewOpen).toBe(true);
  });

  it('applies only once per process: later calls never reopen a closed panel', () => {
    ui.preview.openOnLaunch = true;
    applyStartupPreview();
    ui.quickViewOpen = false; // user closes it
    applyStartupPreview();
    expect(ui.quickViewOpen).toBe(false);
  });

  it('a runtime settings change does not force the panel open', () => {
    applyStartupPreview(); // OFF at startup
    ui.preview.openOnLaunch = true;
    applyStartupPreview();
    expect(ui.quickViewOpen).toBe(false);
  });
});
