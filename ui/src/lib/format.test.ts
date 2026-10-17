import { describe, expect, it } from 'vitest';
import { bytes, duration, percent, serviceUrl, webUrl } from './format';
import { paths } from '@/api/transport';

describe('webUrl (H-12)', () => {
  it('passes web addresses', () => {
    expect(webUrl('http://localhost:4200')).toBe('http://localhost:4200');
    expect(webUrl('https://localhost:4200/x')).toBe('https://localhost:4200/x');
  });

  it('refuses every other scheme', () => {
    for (const u of ['javascript:alert(1)', 'file:///C:/Windows', 'ms-msdt://localhost:1', 'search-ms:query=x', 'not a url', '', null, undefined]) {
      expect(webUrl(u)).toBeUndefined();
    }
  });

  it('falls back to plain http for a service url that is no web address', () => {
    expect(serviceUrl({ url: 'https://localhost:5001', port: 5001 })).toBe('https://localhost:5001');
    expect(serviceUrl({ url: 'javascript:alert(1)', port: 5001 })).toBe('http://localhost:5001');
    expect(serviceUrl({ port: 8080 })).toBe('http://localhost:8080');
  });
});

describe('numbers', () => {
  it('formats sizes, shares and durations', () => {
    expect(bytes(512)).toBe('512 B');
    expect(bytes(1536)).toBe('1.5 KB');
    expect(bytes(null)).toBe('—');
    expect(percent(0.05)).toBe('<0.1%');
    expect(percent(42.4)).toBe('42%');
    expect(duration(61_000)).toBe('1m 1s');
    expect(duration(-5)).toBe('0s');
  });
});

describe('paths.api', () => {
  it('goes to the main tree without a lane', () => {
    expect(paths.api('abc', '/v1/stack')).toBe('/hub/stacks/abc/api/v1/stack');
  });

  it('goes to the lane named, but not for the lane routes', () => {
    expect(paths.api('abc', '/v1/stack', 'feat x')).toBe('/hub/stacks/abc/api/v1/lanes/feat%20x/v1/stack');
    expect(paths.api('abc', '/v1/lanes', 'feat')).toBe('/hub/stacks/abc/api/v1/lanes');
  });
});
