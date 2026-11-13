// The start page: every stack as a tile, in the sidebar's order (favourites
// first, hidden stacks left out), with the hub's figures and the favourite
// programs to open the stack's main tree with.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, nextTick } from 'vue';
import { createPinia, setActivePinia } from 'pinia';
import type { StackEntry } from '@/api/types';
import { FakeTransport, ok } from '@/test/fake';

const t = vi.hoisted(() => ({ current: null as FakeTransport | null }));

vi.mock('@/api', async (importOriginal) => {
  const real = await importOriginal<typeof import('@/api')>();
  return {
    ...real,
    api: () => {
      if (!t.current) throw new Error('no fake transport');
      return t.current;
    },
  };
});

const { router } = await import('@/router');
const { useHub } = await import('@/stores/hub');
const { useFavourites, useHidden } = await import('@/lib/stackOrder');
const { useOpenerPrefs } = await import('@/lib/openers');
const HomeView = (await import('./HomeView.vue')).default;

const stack = (id: string, name: string, more: Partial<StackEntry> = {}): StackEntry => ({
  id,
  name,
  file: `/src/${name}/devstack.toml`,
  repo: `/src/${name}`,
  running: false,
  daemon: null,
  last_seen: null,
  ...more,
});
const shop = stack('ab12', 'shop', { running: true, daemon: { pid: 1, port: 7000, started: Date.now() - 2 * 3_600_000 - 60_000, version: '0.9.8' } });
const crm = stack('cd34', 'crm');
const docs = stack('ef56', 'docs');

let fake: FakeTransport;

beforeEach(() => {
  localStorage.clear();
  setActivePinia(createPinia());
  fake = new FakeTransport();
  t.current = fake;
  fake.on('/hub/session', ok({ version: '0.9.8' }));
  fake.on('/hub/openers', ok([{ id: 'vscode', label: 'VS Code', kind: 'editor' }]));
  fake.on('/hub/open', { status: 204, ok: true, body: null });
  // The lists are the page's, read once: start each test from none.
  for (const id of useHidden().list()) useHidden().show(id);
  for (const id of useFavourites().list()) useFavourites().toggle(id);
  useOpenerPrefs().save({ order: [], hidden: [], favorites: [] });
});

async function settle() {
  for (let i = 0; i < 5; i++) {
    await new Promise((r) => setTimeout(r, 0));
    await nextTick();
  }
}

async function mounted(stacks: StackEntry[]) {
  const hub = useHub();
  await hub.init();
  const events = fake.stream('/hub/events').handlers;
  events.onState?.('open');
  events.onMessage({ type: 'stacks', stacks });
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp(HomeView);
  app.use(router);
  app.mount(el);
  await settle();
  return { el, events, unmount: () => app.unmount() };
}
const names = (el: HTMLElement) => [...el.querySelectorAll('[data-testid="tile-link"]')].map((a) => a.textContent?.trim());
const tile = (el: HTMLElement, id: string) => el.querySelector<HTMLElement>(`[data-testid="stack-tile"][data-id="${id}"]`)!;

describe('the start page', () => {
  it('shows every stack as a tile, favourites first, hidden ones counted apart', async () => {
    useFavourites().toggle('ef56');
    useHidden().hide('cd34');
    const { el, unmount } = await mounted([shop, crm, docs]);
    expect(names(el)).toEqual(['docs', 'shop']);
    expect(el.querySelector('[data-testid="overview-hidden"]')?.textContent).toContain('1 hidden stack');
    expect(el.querySelector('[data-testid="overview-counts"]')?.textContent).toContain('1 running');
    unmount();
  });

  it('never leaves out a running stack, even a hidden one', async () => {
    useHidden().hide('ab12');
    const { el, unmount } = await mounted([shop, crm]);
    expect(names(el)).toEqual(['crm', 'shop']);
    expect(el.querySelector('[data-testid="overview-hidden"]')).toBeNull();
    unmount();
  });

  it("shows the hub's figures and what needs attention", async () => {
    const { el, events, unmount } = await mounted([shop, crm]);
    events.onMessage({
      type: 'summary',
      stacks: [
        { id: 'ab12', state: 'attention', ready: 2, total: 3, lanes: 1 },
        { id: 'cd34', state: 'stopped', ready: 0, total: 0, lanes: 0 },
      ],
    });
    events.onMessage({
      type: 'attention',
      items: [
        { stack: 'ab12', lane: null, service: 'api', text: 'api: failed', detail: 'exit 1', level: 'error' },
        { stack: 'ab12', lane: 'feat', service: 'web', text: 'web: build held for promote', detail: '', level: 'warn' },
        { stack: 'ab12', lane: null, service: 'worker', text: 'worker: failed', detail: '', level: 'error' },
      ],
    });
    await settle();
    const s = tile(el, 'ab12');
    expect(s.querySelector('[data-testid="tile-state"]')?.textContent).toContain('Needs attention');
    expect(s.querySelector('[data-testid="tile-services"]')?.textContent?.replace(/\s+/g, '')).toContain('2/3');
    expect(s.querySelector('[data-testid="tile-lanes"]')?.textContent).toContain('1');
    expect(s.querySelector('[data-testid="tile-up"]')?.textContent).toContain('2h');
    expect([...s.querySelectorAll('[data-testid="tile-attention"]')].map((x) => x.textContent?.trim())).toEqual(['api: failed', 'feat: web: build held for promote']);
    expect(s.textContent).toContain('+1 more');
    const c = tile(el, 'cd34');
    expect(c.querySelector('[data-testid="tile-state"]')?.textContent).toContain('Stopped');
    expect(c.querySelector('[data-testid="tile-services"]')).toBeNull();
    unmount();
  });

  it("opens the stack's main tree with a favourite program", async () => {
    useOpenerPrefs().save({ order: [], hidden: [], favorites: ['vscode'] });
    const { el, unmount } = await mounted([shop]);
    await settle();
    tile(el, 'ab12').querySelector<HTMLElement>('[data-testid="path-favorite"]')!.click();
    await settle();
    expect(fake.requests.find((r) => r.path === '/hub/open')?.body).toEqual({ stack: 'ab12', path: '/src/shop', with: 'vscode' });
    unmount();
  });

  it('marks a favourite from its tile, which moves it first', async () => {
    const { el, unmount } = await mounted([shop, crm]);
    expect(names(el)).toEqual(['crm', 'shop']);
    tile(el, 'ab12').querySelector<HTMLElement>('[data-testid="tile-star"]')!.click();
    await settle();
    expect(names(el)).toEqual(['shop', 'crm']);
    unmount();
  });

  it('the name is the link to the stack page', async () => {
    const { el, unmount } = await mounted([shop]);
    expect(tile(el, 'ab12').querySelector('[data-testid="tile-link"]')?.getAttribute('href')).toBe('#/s/ab12');
    unmount();
  });
});
