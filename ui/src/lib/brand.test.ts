import { describe, expect, it } from 'vitest';
import { attentionTone, faviconHref, faviconSvg } from './brand';

const none = { unreachable: false, items: [], poolWaiting: 0, skillsWaiting: 0 };

describe('attentionTone', () => {
  it('is none when nothing needs attention', () => {
    expect(attentionTone(none)).toBeNull();
  });
  it('is yellow for what waits for the user', () => {
    expect(attentionTone({ ...none, items: [{ level: 'warn' }] })).toBe('warn');
    expect(attentionTone({ ...none, poolWaiting: 2 })).toBe('warn');
    expect(attentionTone({ ...none, skillsWaiting: 1 })).toBe('warn');
  });
  it('is red for a failure or a hub out of reach, whatever else waits', () => {
    expect(attentionTone({ ...none, items: [{ level: 'warn' }, { level: 'error' }], poolWaiting: 1 })).toBe('error');
    expect(attentionTone({ ...none, unreachable: true })).toBe('error');
  });
});

describe('favicon', () => {
  it('shows the mark or the nut, with the dot only when there is one', () => {
    expect(faviconSvg('mark', null)).toContain('M136 176h240');
    expect(faviconSvg('mark', null)).not.toContain('cx="400"');
    expect(faviconSvg('nut', null)).toContain('M4.93 4.93');
    expect(faviconSvg('mark', 'error')).toContain('fill="#f2667a"');
    expect(faviconSvg('nut', 'warn')).toContain('fill="#f0b043"');
  });
  it('is a data URL', () => {
    expect(faviconHref('mark', null).startsWith('data:image/svg+xml,%3Csvg')).toBe(true);
  });
});
