import { describe, expect, it } from 'vitest';
import { filterForm, filterFrom, filterSummary, veilqLinear } from './filters';

describe('filters', () => {
  it('turns the veilq preset into a filter with a write-back', () => {
    const f = filterFrom(veilqLinear('linear'));
    expect(f).toEqual({
      upstream: 'linear',
      tool: 'get_issue',
      paths: ['description'],
      command: ['veilq', 'mark', '-'],
      write: { tool: 'save_issue', arguments: { id: '$args.id', description: '$write' } },
    });
    expect(filterSummary(f)).toBe('get_issue: description through veilq mark - · stored back with save_issue');
  });

  it('round-trips a filter through the form', () => {
    const f = { upstream: 'default:tracker', tool: 'list_issues', paths: ['issues[*].description', 'issues[*].title'], command: ['C:\\Program Files\\veilq\\veilq.exe', 'mark', '-'] };
    expect(filterFrom(filterForm(f))).toEqual(f);
  });

  it('leaves the write-back out without a tool', () => {
    const form = { ...veilqLinear('linear'), writeTool: ' ' };
    expect(filterFrom(form).write).toBeUndefined();
  });
});
