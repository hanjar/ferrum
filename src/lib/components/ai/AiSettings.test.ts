import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';

const invokeMock = vi.fn<(command: string, args?: unknown) => Promise<unknown>>();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (c: string, a?: unknown) => invokeMock(c, a) }));
vi.mock('../../ai/aiRuntime', () => ({ reloadAiRuntime: vi.fn(), loadProviderSetup: vi.fn() }));

import AiSettings from './AiSettings.svelte';
import { OUTBOUND_DISCLOSURE } from '../../ai/provider-setup';
import { SETUP_STORAGE_KEY } from '../../ai/providerSettings';
import { resetSetupStoreForTests } from '../../ai/providerSetupStore';

const KEY = 'sk-test-value-abcdef123456';
const STORED = {
  endpoint: 'http://localhost:20128/v1',
  model: 'm1',
  timeoutMs: 45000,
  credentialRef: 'Ferrum/AI/custom',
  structured: 'none',
};
let configured = false;

beforeEach(() => {
  localStorage.clear();
  resetSetupStoreForTests();
  configured = false;
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command) => {
    if (command === 'ai_credential_status') return { configured };
    if (command === 'ai_credential_save') {
      configured = true;
      return { configured };
    }
    if (command === 'ai_credential_clear') {
      configured = false;
      return { configured };
    }
    if (command === 'ai_test_connection') return { ok: true };
    throw new Error('unexpected ' + command);
  });
});
afterEach(cleanup);

async function fill(endpoint = 'http://127.0.0.1:20128/v1', model = 'glm-4.6') {
  await fireEvent.input(screen.getByLabelText('Endpoint'), { target: { value: endpoint } });
  await fireEvent.input(screen.getByLabelText('Model'), { target: { value: model } });
}
const button = (name: string) => screen.getByRole('button', { name }) as HTMLButtonElement;
const keyField = () => screen.getByLabelText('API key') as HTMLInputElement;
const keyStatus = () => screen.getByRole('status', { name: 'Key status' }).textContent ?? '';

async function saveAll() {
  await fill();
  await fireEvent.input(keyField(), { target: { value: KEY } });
  await fireEvent.click(button('Save'));
  await waitFor(() => expect(keyStatus()).toContain('Key stored'));
}

describe('AiSettings (FER-412, order FER-429)', () => {
  it('lays out endpoint, model, key, Save, Test connection in that order', () => {
    const { container } = render(AiSettings, {});
    const order = [
      screen.getByLabelText('Endpoint'),
      screen.getByLabelText('Model'),
      keyField(),
      button('Save'),
      button('Test connection'),
    ];
    for (let i = 1; i < order.length; i++) {
      // DOCUMENT_POSITION_FOLLOWING: elemen berikutnya muncul setelah elemen sebelumnya.
      expect(
        order[i - 1].compareDocumentPosition(order[i]) & Node.DOCUMENT_POSITION_FOLLOWING,
      ).toBeTruthy();
    }
    expect(container.querySelectorAll('button[type="submit"]').length).toBe(1);
  });

  it('shows labelled fields, the outbound disclosure and an unconfigured key status', async () => {
    render(AiSettings, {});
    for (const label of ['Endpoint', 'Model', 'Timeout (ms)', 'Structured output', 'API key']) {
      expect(screen.getByLabelText(label)).toBeTruthy();
    }
    for (const line of OUTBOUND_DISCLOSURE) expect(screen.getByText(line)).toBeTruthy();
    await waitFor(() => expect(keyStatus()).toContain('No key stored'));
    expect(keyField().type).toBe('password');
    expect(button('Clear key').disabled).toBe(true);
    expect(screen.getByRole('status', { name: 'Save state' }).textContent).toContain(
      'Not saved yet',
    );
  });

  it('prefills from a stored setup and reports it as saved', async () => {
    localStorage.setItem(SETUP_STORAGE_KEY, JSON.stringify(STORED));
    render(AiSettings, {});
    expect((screen.getByLabelText('Endpoint') as HTMLInputElement).value).toBe(
      'http://localhost:20128/v1',
    );
    expect((screen.getByLabelText('Model') as HTMLInputElement).value).toBe('m1');
    expect((screen.getByLabelText('Timeout (ms)') as HTMLInputElement).value).toBe('45000');
    expect((screen.getByLabelText('Structured output') as HTMLSelectElement).value).toBe('none');
    expect(screen.getByRole('status', { name: 'Save state' }).textContent).toContain(
      'All changes saved',
    );
  });

  it('saves setup and key with one Save, clears the field and never renders or stores the key', async () => {
    const onchange = vi.fn<(state: { setupSaved: boolean; keyConfigured: boolean }) => void>();
    const { container } = render(AiSettings, { onchange });
    await saveAll();
    expect(invokeMock).toHaveBeenCalledWith('ai_credential_save', { value: KEY });
    expect(JSON.parse(localStorage.getItem(SETUP_STORAGE_KEY)!).model).toBe('glm-4.6');
    expect(screen.getByRole('status', { name: 'Save result' }).textContent).toContain('Key stored');
    expect(screen.getByRole('status', { name: 'Save state' }).textContent).toContain(
      'All changes saved',
    );
    expect(keyField().value).toBe('');
    expect(container.innerHTML).not.toContain(KEY);
    expect(JSON.stringify(Object.entries(localStorage))).not.toContain(KEY);
    expect(onchange).toHaveBeenLastCalledWith({ setupSaved: true, keyConfigured: true });
    expect(button('Clear key').disabled).toBe(false);
  });

  it('saves only the setup when the key field is empty', async () => {
    render(AiSettings, {});
    await fill();
    await fireEvent.click(button('Save'));
    await waitFor(() =>
      expect(screen.getByRole('status', { name: 'Save result' }).textContent).toContain('Saved'),
    );
    expect(invokeMock.mock.calls.some((c) => c[0] === 'ai_credential_save')).toBe(false);
  });

  it('rejects an unsafe endpoint, stores nothing and does not send the key', async () => {
    render(AiSettings, {});
    await fill('http://example.com/v1');
    await fireEvent.input(keyField(), { target: { value: KEY } });
    await fireEvent.click(button('Save'));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('endpoint'));
    expect(localStorage.getItem(SETUP_STORAGE_KEY)).toBeNull();
    expect(invokeMock.mock.calls.some((c) => c[0] === 'ai_credential_save')).toBe(false);
  });

  it('keeps the field empty and shows a safe error when the key is rejected', async () => {
    invokeMock.mockImplementation(async (command) => {
      if (command === 'ai_credential_save') throw { kind: 'invalid_value' };
      return { configured: false };
    });
    const { container } = render(AiSettings, {});
    await fill();
    await fireEvent.input(keyField(), { target: { value: 'bad value' } });
    await fireEvent.click(button('Save'));
    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toContain('Key was not accepted'),
    );
    expect(keyField().value).toBe('');
    expect(container.innerHTML).not.toContain('bad value');
  });

  it('reports an unsupported platform for the credential store', async () => {
    invokeMock.mockImplementation(async (command) => {
      if (command === 'ai_credential_save') throw { kind: 'unsupported' };
      return { configured: false };
    });
    render(AiSettings, {});
    await fill();
    await fireEvent.input(keyField(), { target: { value: KEY } });
    await fireEvent.click(button('Save'));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('not supported'));
  });

  it('clears the stored key', async () => {
    configured = true;
    render(AiSettings, {});
    await waitFor(() => expect(keyStatus()).toContain('Key stored'));
    await fireEvent.click(button('Clear key'));
    await waitFor(() => expect(keyStatus()).toContain('No key stored'));
    expect(invokeMock).toHaveBeenCalledWith('ai_credential_clear', undefined);
  });

  it('keeps Test connection disabled until setup and key are saved', async () => {
    render(AiSettings, {});
    await waitFor(() => expect(keyStatus()).toContain('No key stored'));
    expect(button('Test connection').disabled).toBe(true);
    expect(screen.getByLabelText('Test hint').textContent).toContain(
      'Save the endpoint and model first',
    );
    await fill();
    await fireEvent.click(button('Save'));
    expect(button('Test connection').disabled).toBe(true);
    expect(screen.getByLabelText('Test hint').textContent).toContain('Save an API key first');
    await fireEvent.click(button('Test connection'));
    expect(invokeMock.mock.calls.some((c) => c[0] === 'ai_test_connection')).toBe(false);
  });

  it('disables Test connection again while there are unsaved edits', async () => {
    render(AiSettings, {});
    await saveAll();
    expect(button('Test connection').disabled).toBe(false);
    await fireEvent.input(screen.getByLabelText('Model'), { target: { value: 'other' } });
    expect(button('Test connection').disabled).toBe(true);
    expect(screen.getByRole('status', { name: 'Save state' }).textContent).toContain(
      'Unsaved changes',
    );
    expect(screen.getByLabelText('Test hint').textContent).toContain('unsaved changes');
  });

  it('tests the saved values and reports success with the tested model', async () => {
    render(AiSettings, {});
    await saveAll();
    await fireEvent.click(button('Test connection'));
    await waitFor(() =>
      expect(screen.getByRole('status', { name: 'Connection status' }).textContent).toContain(
        'Connection OK: glm-4.6 at http://127.0.0.1:20128/v1',
      ),
    );
    const call = invokeMock.mock.calls.find((c) => c[0] === 'ai_test_connection')!;
    expect(call[1]).toEqual({
      config: {
        endpoint: 'http://127.0.0.1:20128/v1',
        model: 'glm-4.6',
        timeoutMs: 30000,
        credentialRef: 'Ferrum/AI/custom',
        structured: 'json_object',
      },
    });
    expect(JSON.stringify(call[1])).not.toContain(KEY);
  });

  it('shows a safe error code when the connection test fails', async () => {
    configured = true;
    localStorage.setItem(SETUP_STORAGE_KEY, JSON.stringify(STORED));
    invokeMock.mockImplementation(async (command) => {
      if (command === 'ai_test_connection') throw { kind: 'auth', attempts: 1 };
      return { configured: true };
    });
    render(AiSettings, {});
    await waitFor(() => expect(button('Test connection').disabled).toBe(false));
    await fireEvent.click(button('Test connection'));
    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toContain('Connection failed'),
    );
    expect(screen.getByRole('alert').textContent).toContain('(auth)');
  });

  it('can cancel a running connection test', async () => {
    configured = true;
    localStorage.setItem(SETUP_STORAGE_KEY, JSON.stringify(STORED));
    invokeMock.mockImplementation((command) =>
      command === 'ai_test_connection'
        ? new Promise(() => undefined)
        : Promise.resolve({ configured: true }),
    );
    render(AiSettings, {});
    await waitFor(() => expect(button('Test connection').disabled).toBe(false));
    await fireEvent.click(button('Test connection'));
    await fireEvent.click(await screen.findByRole('button', { name: 'Cancel test' }));
    await waitFor(() =>
      expect(screen.getByRole('status', { name: 'Connection status' }).textContent).toContain(
        'cancelled',
      ),
    );
  });

  it('submits Save with Enter inside the form', async () => {
    render(AiSettings, {});
    await fill();
    await fireEvent.submit(screen.getByRole('form', { name: 'AI provider setup' }));
    await waitFor(() => expect(localStorage.getItem(SETUP_STORAGE_KEY)).not.toBeNull());
  });
});

describe('AiSettings with the desktop backend (FER-430)', () => {
  let slot: unknown;
  beforeEach(() => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    resetSetupStoreForTests();
    slot = { ...STORED, structured: 'json_object' };
    configured = true;
    invokeMock.mockImplementation(async (command, args) => {
      if (command === 'ai_setup_load') return slot ?? null;
      if (command === 'ai_setup_save') {
        slot = (args as { config: unknown }).config;
        return null;
      }
      if (command === 'ai_credential_status') return { configured };
      if (command === 'ai_test_connection') return { ok: true };
      throw new Error('unexpected ' + command);
    });
  });
  afterEach(() => {
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
    resetSetupStoreForTests();
  });

  it('fills endpoint and model from the backend (a setup made in another build) and shows them as saved', async () => {
    render(AiSettings, {});
    await waitFor(() =>
      expect((screen.getByLabelText('Endpoint') as HTMLInputElement).value).toBe(
        'http://localhost:20128/v1',
      ),
    );
    expect((screen.getByLabelText('Model') as HTMLInputElement).value).toBe('m1');
    expect((screen.getByLabelText('Timeout (ms)') as HTMLInputElement).value).toBe('45000');
    await waitFor(() =>
      expect(screen.getByRole('status', { name: 'Save state' }).textContent).toContain(
        'All changes saved',
      ),
    );
    await waitFor(() => expect(button('Test connection').disabled).toBe(false));
    expect(localStorage.getItem(SETUP_STORAGE_KEY)).toBeNull();
  });

  it('does not overwrite what the user already typed when the backend setup arrives late', async () => {
    let release!: (value: unknown) => void;
    invokeMock.mockImplementation((command) =>
      command === 'ai_setup_load'
        ? new Promise((resolve) => {
            release = resolve;
          })
        : Promise.resolve({ configured }),
    );
    render(AiSettings, {});
    await fireEvent.input(screen.getByLabelText('Model'), { target: { value: 'typed-model' } });
    release({ ...STORED, structured: 'json_object' });
    await waitFor(() =>
      expect(screen.getByRole('status', { name: 'Key status' }).textContent).toContain(
        'Key stored',
      ),
    );
    expect((screen.getByLabelText('Model') as HTMLInputElement).value).toBe('typed-model');
  });

  it('saves to the backend and never writes browser storage', async () => {
    render(AiSettings, {});
    await waitFor(() =>
      expect((screen.getByLabelText('Model') as HTMLInputElement).value).toBe('m1'),
    );
    await fireEvent.input(screen.getByLabelText('Model'), { target: { value: 'm2' } });
    await fireEvent.click(button('Save'));
    await waitFor(() => expect((slot as { model: string }).model).toBe('m2'));
    expect(localStorage.getItem(SETUP_STORAGE_KEY)).toBeNull();
    expect(JSON.stringify(invokeMock.mock.calls)).not.toContain(KEY);
  });
});

describe('AiSettings in a test profile (FER-457)', () => {
  it('disables the key field and Clear key with a visible reason', async () => {
    configured = true;
    const base = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command, args) =>
      command === 'test_profile_active' ? true : base(command, args),
    );
    render(AiSettings, {});
    await waitFor(() => expect(keyStatus()).toContain('Key stored'));
    await waitFor(() => expect(keyField().disabled).toBe(true));
    expect(button('Clear key').disabled).toBe(true);
    expect(screen.getByRole('status', { name: 'Test profile' }).textContent).toContain(
      'Profil uji',
    );
    expect(invokeMock.mock.calls.some((c) => c[0] === 'ai_credential_clear')).toBe(false);
  });
});
