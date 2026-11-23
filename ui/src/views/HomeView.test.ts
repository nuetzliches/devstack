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
  sessionStorage.clear();
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

  it('offers no favourite program while the hub is away', async () => {
    useOpenerPrefs().save({ order: [], hidden: [], favorites: ['vscode'] });
    const { el, events, unmount } = await mounted([shop]);
    await settle();
    expect(tile(el, 'ab12').querySelector('[data-testid="path-favorite"]')).not.toBeNull();
    // The tiles stay from the last list; a program could not be opened.
    events.onState?.('error');
    await settle();
    expect(tile(el, 'ab12').querySelector('[data-testid="path-favorite"]')).toBeNull();
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

describe('the search and the state filter of the start page', () => {
  const billing = stack('gh78', 'Billing', { repo: '/work/ledger' });
  const search = (el: HTMLElement) => el.querySelector<HTMLInputElement>('[data-testid="overview-search"]')!;
  const type = async (el: HTMLElement, text: string) => {
    search(el).value = text;
    search(el).dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
  };
  const chip = (el: HTMLElement, key: string) => el.querySelector<HTMLElement>(`[data-testid="overview-chip-${key}"]`)!;
  const attention = (events: { onMessage: (m: unknown) => void }, ...ids: string[]) =>
    events.onMessage({
      type: 'attention',
      items: ids.map((stackId) => ({ stack: stackId, lane: null, service: 'api', text: 'api: failed', detail: '', level: 'error' })),
    });

  it('finds a stack by its name or its repository folder, every word, whatever the case', async () => {
    const { el, unmount } = await mounted([shop, crm, billing]);
    await type(el, 'BILL');
    expect(names(el)).toEqual(['Billing']);
    await type(el, 'ledger');
    expect(names(el)).toEqual(['Billing']);
    await type(el, 'work billing');
    expect(names(el)).toEqual(['Billing']);
    await type(el, 'work shop');
    expect(names(el)).toEqual([]);
    await type(el, '');
    expect(names(el)).toEqual(['Billing', 'crm', 'shop']);
    unmount();
  });

  it('counts the stacks per state and narrows to the ones that run or need attention', async () => {
    const { el, events, unmount } = await mounted([shop, crm, billing]);
    attention(events, 'cd34');
    await settle();
    expect(chip(el, 'all').textContent?.replace(/\s+/g, ' ').trim()).toBe('All 3');
    expect(chip(el, 'running').textContent).toContain('1');
    expect(chip(el, 'attention').textContent).toContain('1');
    chip(el, 'running').click();
    await settle();
    expect(names(el)).toEqual(['shop']);
    expect(chip(el, 'running').getAttribute('aria-pressed')).toBe('true');
    chip(el, 'attention').click();
    await settle();
    expect(names(el)).toEqual(['crm']);
    chip(el, 'all').click();
    await settle();
    expect(names(el)).toEqual(['Billing', 'crm', 'shop']);
    unmount();
  });

  it('combines the state with the text, and says so when nothing matches', async () => {
    const { el, unmount } = await mounted([shop, crm]);
    chip(el, 'running').click();
    await type(el, 'crm');
    expect(names(el)).toEqual([]);
    expect(el.querySelector('[data-testid="overview-tiles"]')).toBeNull();
    expect(el.querySelector('[data-testid="overview-empty"]')?.textContent).toContain('No stack matches');
    el.querySelector<HTMLElement>('[data-testid="overview-clear"]')!.click();
    await settle();
    expect(search(el).value).toBe('');
    expect(chip(el, 'all').getAttribute('aria-pressed')).toBe('true');
    expect(names(el)).toEqual(['crm', 'shop']);
    unmount();
  });

  it('keeps hidden stacks out, and says how many of them the search would have found', async () => {
    useHidden().hide('gh78');
    const { el, unmount } = await mounted([shop, billing]);
    expect(el.querySelector('[data-testid="overview-hidden"]')?.textContent).toContain('1 hidden stack is not shown');
    await type(el, 'bill');
    expect(names(el)).toEqual([]);
    expect(el.querySelector('[data-testid="overview-hidden"]')?.textContent).toContain('1 hidden stack matches');
    await type(el, 'shop');
    expect(el.querySelector('[data-testid="overview-hidden"]')?.textContent).toContain('1 hidden stack is not shown');
    unmount();
  });

  it('jumps to the search with /, and Esc clears it, then leaves it', async () => {
    const { el, unmount } = await mounted([shop, crm]);
    window.dispatchEvent(new KeyboardEvent('keydown', { key: '/', bubbles: true }));
    expect(document.activeElement).toBe(search(el));
    await type(el, 'crm');
    // A `/` typed into a field is text, not a jump.
    const typed = new KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true });
    search(el).dispatchEvent(typed);
    expect(typed.defaultPrevented).toBe(false);
    search(el).dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await settle();
    expect(search(el).value).toBe('');
    expect(document.activeElement).toBe(search(el));
    search(el).dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(document.activeElement).not.toBe(search(el));
    unmount();
  });

  it('is found as it was left when the page is opened again', async () => {
    const first = await mounted([shop, crm]);
    await type(first.el, 'crm');
    chip(first.el, 'attention').click();
    await settle();
    first.unmount();
    // A new page of the same tab: the store is new, the session storage stays.
    setActivePinia(createPinia());
    const again = await mounted([shop, crm]);
    expect(search(again.el).value).toBe('crm');
    expect(chip(again.el, 'attention').getAttribute('aria-pressed')).toBe('true');
    again.unmount();
  });
});

describe('the order of the start page', () => {
  const alpha = stack('aa01', 'alpha', { last_commit: 100 });
  const beta = stack('bb02', 'beta', { last_commit: 200 });
  const gamma = stack('cc03', 'gamma', { last_commit: 50 });
  const choose = async (el: HTMLElement, order: string) => {
    el.querySelector<HTMLElement>('[data-testid="overview-order"]')!.click();
    await settle();
    document.querySelector<HTMLElement>(`[data-testid="overview-order-menu"] [data-order="${order}"]`)!.click();
    await settle();
  };

  it('orders the tiles by name, or by last edited, and keeps the choice in the sidebar’s place', async () => {
    const { el, unmount } = await mounted([gamma, alpha, beta]);
    expect(names(el)).toEqual(['alpha', 'beta', 'gamma']);
    await choose(el, 'edited');
    expect(names(el)).toEqual(['beta', 'alpha', 'gamma']);
    expect(localStorage.getItem('devstack.sidebar.order')).toBe('"edited"');
    // The menu is closed by a choice, and shows it ticked when opened again.
    expect(document.querySelector('[data-testid="overview-order-menu"]')).toBeNull();
    el.querySelector<HTMLElement>('[data-testid="overview-order"]')!.click();
    await settle();
    expect(document.querySelector('[data-testid="overview-order-menu"] [aria-checked="true"]')?.getAttribute('data-order')).toBe('edited');
    unmount();
  });

  it('is the order the sidebar chose, and the list follows it until a column is chosen', async () => {
    localStorage.setItem('devstack.sidebar.order', '"edited"');
    const { el, unmount } = await mounted([gamma, alpha, beta]);
    expect(names(el)).toEqual(['beta', 'alpha', 'gamma']);
    el.querySelector<HTMLElement>('[data-testid="overview-view-list"]')!.click();
    await settle();
    expect(names(el)).toEqual(['beta', 'alpha', 'gamma']);
    await choose(el, 'name');
    expect(names(el)).toEqual(['alpha', 'beta', 'gamma']);
    unmount();
  });

  it('offers to update the order when a commit would move a tile, and moves nothing before', async () => {
    const { el, events, unmount } = await mounted([gamma, alpha, beta]);
    const refresh = () => el.querySelector<HTMLElement>('[data-testid="overview-order-refresh"]');
    await choose(el, 'edited');
    expect(refresh()).toBeNull();
    events.onMessage({ type: 'stacks', stacks: [gamma, stack('aa01', 'alpha', { last_commit: 900 }), beta] });
    await settle();
    expect(names(el)).toEqual(['beta', 'alpha', 'gamma']);
    refresh()!.click();
    await settle();
    expect(names(el)).toEqual(['alpha', 'beta', 'gamma']);
    expect(refresh()).toBeNull();
    unmount();
  });

  it('puts the favourites first whatever the order', async () => {
    useFavourites().toggle('cc03');
    const { el, unmount } = await mounted([gamma, alpha, beta]);
    await choose(el, 'edited');
    expect(names(el)).toEqual(['gamma', 'beta', 'alpha']);
    unmount();
  });
});

describe('the views of the start page', () => {
  const billing = stack('gh78', 'Billing', { repo: '/work/ledger' });
  const view = (el: HTMLElement, key: string) => el.querySelector<HTMLElement>(`[data-testid="overview-view-${key}"]`)!;
  const rowOf = (el: HTMLElement, id: string) => el.querySelector<HTMLElement>(`[data-testid="stack-row"][data-id="${id}"]`)!;
  const cell = (row: HTMLElement, id: string) => row.querySelector(`[data-testid="${id}"]`)?.textContent?.replace(/\s+/g, ' ').trim();
  const head = (el: HTMLElement, label: string) => [...el.querySelectorAll('th')].find((th) => th.textContent?.includes(label))!.querySelector('button')!;
  const glimpse = (events: { onMessage: (m: unknown) => void }) => {
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
        { stack: 'ab12', lane: null, service: 'api', text: 'api: failed', detail: '', level: 'error' },
        { stack: 'ab12', lane: 'feat', service: 'web', text: 'web: build held for promote', detail: '', level: 'warn' },
        { stack: 'ab12', lane: null, service: 'worker', text: 'worker: failed', detail: '', level: 'error' },
      ],
    });
  };

  it('starts with large tiles, switches to small tiles, and finds the choice again', async () => {
    const first = await mounted([shop, crm]);
    expect(view(first.el, 'large').getAttribute('aria-pressed')).toBe('true');
    expect(first.el.querySelector('[data-testid="overview-tiles"]')?.classList.contains('small')).toBe(false);
    view(first.el, 'small').click();
    await settle();
    expect(first.el.querySelector('[data-testid="overview-tiles"]')?.classList.contains('small')).toBe(true);
    first.unmount();
    // Another tab of this browser: a new page and store, the same local storage.
    setActivePinia(createPinia());
    sessionStorage.clear();
    const again = await mounted([shop, crm]);
    expect(view(again.el, 'small').getAttribute('aria-pressed')).toBe('true');
    expect(again.el.querySelector('[data-testid="overview-tiles"]')?.classList.contains('small')).toBe(true);
    again.unmount();
  });

  it('shows less on a small tile: no time up, only the first thing that needs attention', async () => {
    const { el, events, unmount } = await mounted([shop, crm]);
    glimpse(events);
    await settle();
    expect(tile(el, 'ab12').querySelector('[data-testid="tile-up"]')).not.toBeNull();
    view(el, 'small').click();
    await settle();
    const s = tile(el, 'ab12');
    expect(s.querySelector('[data-testid="tile-up"]')).toBeNull();
    expect(s.querySelector('[data-testid="tile-services"]')?.textContent?.replace(/\s+/g, '')).toContain('2/3');
    expect([...s.querySelectorAll('[data-testid="tile-attention"]')].map((x) => x.textContent?.trim())).toEqual(['api: failed']);
    expect(s.textContent).toContain('+2 more');
    unmount();
  });

  it('lists a row per stack in the tiles order, with the same figures', async () => {
    useFavourites().toggle('ab12');
    const { el, events, unmount } = await mounted([crm, shop, docs]);
    glimpse(events);
    view(el, 'list').click();
    await settle();
    expect(el.querySelector('[data-testid="overview-tiles"]')).toBeNull();
    expect(names(el)).toEqual(['shop', 'crm', 'docs']);
    const s = rowOf(el, 'ab12');
    expect(cell(s, 'tile-state')).toContain('Needs attention');
    expect(cell(s, 'tile-services')?.replace(/\s/g, '')).toBe('2/3');
    expect(cell(s, 'tile-lanes')).toBe('1');
    expect(cell(s, 'tile-up')).toContain('2h');
    expect(cell(s, 'row-attention')).toBe('3');
    expect(s.querySelector('[data-testid="row-attention"]')?.getAttribute('title')).toBe('api: failed\nfeat: web: build held for promote\nworker: failed');
    const c = rowOf(el, 'cd34');
    expect(cell(c, 'tile-state')).toContain('Stopped');
    expect(cell(c, 'tile-services')).toBe('—');
    expect(cell(c, 'row-attention')).toBe('—');
    unmount();
  });

  it('sorts the list by a column, turns it round, and goes back to the tiles order', async () => {
    useFavourites().toggle('ab12');
    const { el, unmount } = await mounted([crm, shop, docs]);
    view(el, 'list').click();
    await settle();
    expect(names(el)).toEqual(['shop', 'crm', 'docs']);
    head(el, 'Name').click();
    await settle();
    expect(names(el)).toEqual(['crm', 'docs', 'shop']);
    head(el, 'Name').click();
    await settle();
    expect(names(el)).toEqual(['shop', 'docs', 'crm']);
    head(el, 'Name').click();
    await settle();
    expect(names(el)).toEqual(['shop', 'crm', 'docs']);
    unmount();
  });

  it('sorts by what needs attention, the most first', async () => {
    const { el, events, unmount } = await mounted([shop, crm, docs]);
    glimpse(events);
    view(el, 'list').click();
    await settle();
    head(el, 'Attention').click();
    await settle();
    expect(names(el)[0]).toBe('shop');
    unmount();
  });

  it('applies the search and the state to the list as to the tiles', async () => {
    const { el, unmount } = await mounted([shop, crm, billing]);
    view(el, 'list').click();
    await settle();
    const search = el.querySelector<HTMLInputElement>('[data-testid="overview-search"]')!;
    search.value = 'ledger';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    expect(names(el)).toEqual(['Billing']);
    el.querySelector<HTMLElement>('[data-testid="overview-chip-running"]')!.click();
    await settle();
    expect(names(el)).toEqual([]);
    expect(el.querySelector('[data-testid="overview-list"]')).toBeNull();
    expect(el.querySelector('[data-testid="overview-empty"]')).not.toBeNull();
    unmount();
  });

  it('marks a favourite and opens a program from a row, and the row opens the stack', async () => {
    useOpenerPrefs().save({ order: [], hidden: [], favorites: ['vscode'] });
    const { el, unmount } = await mounted([shop, crm]);
    view(el, 'list').click();
    await settle();
    const push = vi.spyOn(router, 'push').mockResolvedValue(undefined);
    rowOf(el, 'ab12').querySelector<HTMLElement>('[data-testid="path-favorite"]')!.click();
    await settle();
    expect(fake.requests.find((r) => r.path === '/hub/open')?.body).toEqual({ stack: 'ab12', path: '/src/shop', with: 'vscode' });
    rowOf(el, 'ab12').querySelector<HTMLElement>('[data-testid="tile-star"]')!.click();
    await settle();
    expect(names(el)).toEqual(['shop', 'crm']);
    // The star and the program do not open the stack; the row does.
    expect(push).not.toHaveBeenCalled();
    rowOf(el, 'cd34').querySelector<HTMLElement>('[data-testid="tile-commit"]')!.click();
    expect(push).toHaveBeenCalledWith({ name: 'stack', params: { stack: 'cd34' } });
    push.mockRestore();
    unmount();
  });
});
