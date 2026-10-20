import { describe, expect, it } from 'vitest';
import { ContractError } from './contracts';
import {
  classifyProviderFailure,
  describeProviderError,
  endpointHostPort,
  providerSaid,
  uiCodeOf,
  type ProviderUiCode,
} from './providerErrors';
import { RemoteProviderError } from './remote-provider';

describe('endpointHostPort', () => {
  it('keeps only host:port', () => {
    expect(endpointHostPort('http://127.0.0.1:20128/v1')).toBe('127.0.0.1:20128');
    expect(endpointHostPort('https://api.example.com/api/paas/v4')).toBe('api.example.com');
    expect(endpointHostPort('https://user:sk-secret@host.example:8443/v1?k=1')).toBe(
      'host.example:8443',
    );
    expect(endpointHostPort('nonsense')).toBeUndefined();
    expect(endpointHostPort(undefined)).toBeUndefined();
  });
});

describe('FER-573: providerSaid / detail', () => {
  it('formats the provider message with the model and ignores empty or unsafe input', () => {
    expect(providerSaid('404 Requested entity was not found', 'ag/gemini-4-argon-high')).toBe(
      'Provider said: 404 Requested entity was not found (model ag/gemini-4-argon-high)',
    );
    expect(providerSaid('boom')).toBe('Provider said: boom');
    expect(providerSaid('boom', 'bad model name!')).toBe('Provider said: boom');
    expect(providerSaid(undefined, 'm')).toBeUndefined();
    expect(providerSaid('   ', 'm')).toBeUndefined();
    expect(providerSaid('key sk-abcdef0123456789')).toBe('Provider said: key [redacted]');
  });

  it('keeps the code, message and actions unchanged and only adds the detail line', () => {
    const plain = describeProviderError('server_error', { hostPort: '127.0.0.1:20128' });
    const withDetail = describeProviderError('server_error', {
      hostPort: '127.0.0.1:20128',
      detail: 'upstream overloaded',
    });
    expect(plain.detail).toBeUndefined();
    expect(withDetail.detail).toBe('Provider said: upstream overloaded');
    expect({ ...withDetail, detail: undefined }).toEqual({ ...plain, detail: undefined });
  });
});

describe('describeProviderError', () => {
  const all: ProviderUiCode[] = [
    'unreachable',
    'timeout',
    'auth',
    'credential',
    'model_not_found',
    'bad_request',
    'rate_limited',
    'quota_exceeded',
    'server_error',
    'invalid_response',
    'not_supported',
    'invalid_config',
    'unknown',
  ];

  it('gives every code an Indonesian message ending in "Tidak ada yang dijalankan."', () => {
    for (const code of all) {
      const view = describeProviderError(code, { hostPort: '127.0.0.1:20128' });
      expect(view.code).toBe(code);
      expect(view.message.endsWith('Tidak ada yang dijalankan.')).toBe(true);
    }
  });

  it('unreachable: names host:port and offers retry, settings and test connection', () => {
    const view = describeProviderError('unreachable', { hostPort: '127.0.0.1:20128' });
    expect(view.message).toContain('Server AI tidak bisa dihubungi (127.0.0.1:20128)');
    expect(view.actions).toEqual(['retry', 'settings', 'test']);
  });

  it('auth and model errors point to Settings, rate limits offer retry', () => {
    expect(describeProviderError('auth').actions).toContain('settings');
    expect(describeProviderError('auth').actions).not.toContain('retry');
    expect(describeProviderError('model_not_found').message).toContain('model');
    expect(describeProviderError('rate_limited').actions).toContain('retry');
    expect(describeProviderError('server_error').actions).toContain('retry');
  });

  it('invalid_response is distinct from a dead server', () => {
    const view = describeProviderError('invalid_response');
    expect(view.message).toContain('tidak bisa dipahami');
    expect(view.message).not.toContain('tidak bisa dihubungi');
  });

  it('not_supported lists the offered capabilities and has no buttons', () => {
    const view = describeProviderError('not_supported', {
      capabilities: ['archive.create', 'activity.query', 'bogus.id'],
    });
    expect(view.message).toContain('arsip ZIP');
    expect(view.message).toContain('riwayat');
    expect(view.message).not.toContain('bogus');
    expect(view.actions).toEqual([]);
  });
});

describe('classification', () => {
  it('maps backend kinds to UI codes', () => {
    expect(uiCodeOf('network')).toBe('unreachable');
    expect(uiCodeOf('model_unavailable')).toBe('model_not_found');
    expect(uiCodeOf('malformed')).toBe('invalid_response');
    expect(uiCodeOf('refused')).toBe('not_supported');
    expect(uiCodeOf('unsafe_endpoint')).toBe('invalid_config');
    expect(uiCodeOf('backend_failure')).toBe('unknown');
  });

  it('classifies thrown errors without reading their messages', () => {
    expect(classifyProviderFailure(new RemoteProviderError('timeout', 'sk-LEAK'))).toBe('timeout');
    expect(classifyProviderFailure(new ContractError('invalid_payload', 'x'))).toBe(
      'invalid_response',
    );
    expect(classifyProviderFailure(new Error('boom'))).toBe('unknown');
  });
});
