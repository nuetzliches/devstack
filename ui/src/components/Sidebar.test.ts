// The routes of a stack and its lanes (ADR-0081), and the sidebar on them:
// the stack's row opens its page, each tree its lane's views, the highlight
// follows the route; older links land on a lane's view.
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
const { persist } = await import('@/lib/persist');
const Sidebar = (await import('./Sidebar.vue')).default;

const entry: StackEntry = { id: 'ab12', name: 'shop', file: '/src/shop/devstack.toml', repo: '/src/shop', running: true, daemon: { pid: 1, port: 7000, started: 0, version: '0.9.8' }, last_seen: null };
const feat = { name: 'feat', path: '/src/feat', branch: 'feat', number: 1, active: true, port_offset: 100, owns_main: false, running: 1, services: 1 };

let fake: FakeTransport;

beforeEach(() => {
  localStorage.clear();
  setActivePinia(createPinia());
  fake = new FakeTransport();
  t.current = fake;
  fake.on('/hub/session', ok({ version: '0.9.8' }));
  fake.on('/hub/stacks/ab12/api/v1/lanes', ok([feat]));
});

async function settle() {
  for (let i = 0; i < 5; i++) {
    await new Promise((r) => setTimeout(r, 0));
    await nextTick();
  }
}

describe('routes of a stack and its lanes', () => {
  it('a stack is a page; a lane, main too, has the views', () => {
    expect(router.resolve('/s/ab12').name).toBe('stack');
    expect(router.resolve('/s/ab12/l/main').name).toBe('overview');
    expect(router.resolve('/s/ab12/l/feat/services/api')).toMatchObject({ name: 'service', params: { stack: 'ab12', lane: 'feat', name: 'api' } });
    expect(router.resolve('/s/ab12/l/feat/usage').name).toBe('usage');
  });

  it('an older link lands on the lane it named, or the one shown last', async () => {
    await router.push('/s/ab12/services/api?lane=feat');
    expect(router.currentRoute.value.fullPath).toBe('/s/ab12/l/feat/services/api');
    await router.push('/s/ab12?lane=');
    expect(router.currentRoute.value.fullPath).toBe('/s/ab12/l/main');
    persist.set('lane.ab12', 'feat');
    await router.push('/s/ab12/usage');
    expect(router.currentRoute.value.fullPath).toBe('/s/ab12/l/feat/usage');
    await router.push('/s/ab12');
    expect(router.currentRoute.value.name).toBe('stack');
  });
});

describe('the sidebar', () => {
  async function mounted() {
    const hub = useHub();
    await hub.init();
    fake.stream('/hub/events').handlers.onMessage({ type: 'stacks', stacks: [entry] });
    const el = document.createElement('div');
    document.body.appendChild(el);
    const app = createApp(Sidebar);
    app.use(router);
    app.mount(el);
    await settle();
    return { el, unmount: () => app.unmount() };
  }
  const q = (el: HTMLElement, sel: string) => el.querySelector<HTMLElement>(sel)!;

  it('lists main first, then the lanes, and highlights what the route shows', async () => {
    await router.push('/s/ab12');
    const { el, unmount } = await mounted();
    const rows = [...el.querySelectorAll<HTMLElement>('[data-testid="sidebar-lane-main"], [data-testid="sidebar-lane"]')];
    expect(rows.map((r) => r.querySelector('.label')?.textContent?.trim())).toEqual(['main', 'feat']);
    expect(q(el, '[data-testid="sidebar-stack"]').classList.contains('active')).toBe(true);
    expect(rows.some((r) => r.classList.contains('active'))).toBe(false);

    await router.push('/s/ab12/l/feat/usage');
    await settle();
    expect(q(el, '[data-testid="sidebar-stack"]').classList.contains('active')).toBe(false);
    expect(q(el, '[data-testid="sidebar-lane"][data-lane="feat"]').classList.contains('active')).toBe(true);
    expect(q(el, '[data-testid="sidebar-lane-main"]').classList.contains('active')).toBe(false);
    unmount();
  });

  it('opens the stack page from the stack, a lane view from a tree, on the tab shown', async () => {
    await router.push('/s/ab12/l/feat/events');
    const { el, unmount } = await mounted();
    q(el, '[data-testid="sidebar-lane-main"]').click();
    await settle();
    expect(router.currentRoute.value.fullPath).toBe('/s/ab12/l/main/events');
    q(el, '[data-testid="sidebar-stack"]').click();
    await settle();
    expect(router.currentRoute.value.fullPath).toBe('/s/ab12');
    q(el, '[data-testid="sidebar-lane"][data-lane="feat"]').click();
    await settle();
    expect(router.currentRoute.value.fullPath).toBe('/s/ab12/l/feat/events');
    unmount();
  });
});
