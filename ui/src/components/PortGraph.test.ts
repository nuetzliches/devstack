// The stack page's port graph (ADR-0082 point 2): an overlap with another
// stack is red and names it; instance ports show on request.
import { describe, expect, it } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import type { PortEntry, PortOverlap } from '@/api/types';
import PortGraph from './PortGraph.vue';

const entries: PortEntry[] = [
  { port: 4200, proto: 'tcp', lane: 'main', kind: 'fixed', holder: 'web', via: 'forward', target: 51200, listening: true },
  { port: 51200, proto: 'tcp', lane: 'main', kind: 'instance', holder: 'web', via: 'direct', listening: true },
  { port: 5080, proto: 'tcp', lane: 'main', kind: 'fixed', holder: 'api', via: 'proxy', target: 49010, listening: false },
];
const overlaps: PortOverlap[] = [
  {
    port: 4200,
    proto: 'tcp',
    stacks: [
      { id: 'shop-1', name: 'shop', lane: 'main', holder: 'web', kind: 'fixed' },
      { id: 'docs-1', name: 'docs', lane: 'main', holder: 'site', kind: 'declared' },
    ],
  },
];

async function mount() {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp({
    render: () => h(PortGraph, { stack: 'shop-1', entries, overlaps, lanes: [{ name: 'main' }, { name: 'feat' }], servedBy: 'main', running: true }),
  });
  app.mount(el);
  await nextTick();
  return { el, unmount: () => (app.unmount(), el.remove()) };
}

describe('the ports card', () => {
  it('draws an overlap red, naming the other stack, and counts it in the header', async () => {
    const { el, unmount } = await mount();
    const port = el.querySelector<HTMLElement>('[data-testid="stack-ports-graph"] [data-testid="stack-port"][data-port="4200/tcp"]')!;
    expect(port).not.toBeNull();
    expect(port.classList.contains('pg-overlap')).toBe(true);
    expect(port.getAttribute('aria-label')).toContain('docs (lane main, site, declared)');
    expect(el.querySelector('[data-testid="stack-ports-overlaps"]')?.textContent?.trim()).toBe('1 overlap');
    expect(el.querySelector('[data-testid="stack-ports-shared"]')?.textContent).toContain('also used by docs');
    // Not this stack's overlap: plain; not listening: muted.
    const api = el.querySelector<HTMLElement>('[data-testid="stack-ports-graph"] [data-port="5080/tcp"]')!;
    expect(api.classList.contains('pg-overlap')).toBe(false);
    expect(api.classList.contains('pg-muted')).toBe(true);
    // A lane without ports is one line.
    expect(el.querySelector('[data-testid="stack-ports-idle"]')?.textContent).toContain('feat');
    unmount();
  });

  it('hides instance ports until the toggle shows them', async () => {
    const { el, unmount } = await mount();
    const inGraph = () => el.querySelector('[data-testid="stack-ports-graph"] [data-port="51200/tcp"]');
    expect(inGraph()).toBeNull();
    el.querySelector<HTMLElement>('[data-testid="stack-ports-instance"]')!.click();
    await nextTick();
    expect(inGraph()).not.toBeNull();
    unmount();
  });

  it('highlights the path of a port on focus', async () => {
    const { el, unmount } = await mount();
    const port = el.querySelector<HTMLElement>('[data-testid="stack-ports-graph"] [data-port="4200/tcp"]')!;
    port.dispatchEvent(new FocusEvent('focus'));
    await nextTick();
    const web = [...el.querySelectorAll<HTMLElement>('[data-testid="stack-ports-holder"]')].find((x) => x.textContent?.includes('web'))!;
    const api = [...el.querySelectorAll<HTMLElement>('[data-testid="stack-ports-holder"]')].find((x) => x.textContent?.includes('api'))!;
    expect(web.classList.contains('pg-on')).toBe(true);
    expect(api.classList.contains('pg-dim')).toBe(true);
    unmount();
  });
});
