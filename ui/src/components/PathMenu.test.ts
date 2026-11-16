// A tree's path with its menu: a favourite program's button beside it opens
// the path and closes the menu, as the menu's own programs do.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import { createPinia, setActivePinia } from 'pinia';
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

const { useOpenerPrefs } = await import('@/lib/openers');
const PathMenu = (await import('./PathMenu.vue')).default;

let fake: FakeTransport;

beforeEach(() => {
  localStorage.clear();
  setActivePinia(createPinia());
  fake = new FakeTransport();
  t.current = fake;
  fake.on('/hub/openers', ok([{ id: 'vscode', label: 'VS Code', kind: 'editor' }]));
  fake.on('/hub/open', { status: 204, ok: true, body: null });
  useOpenerPrefs().save({ order: [], hidden: [], favorites: ['vscode'] });
});

async function settle() {
  for (let i = 0; i < 5; i++) {
    await new Promise((r) => setTimeout(r, 0));
    await nextTick();
  }
}

describe('PathMenu', () => {
  it('closes its menu when a favourite program opens the path', async () => {
    const el = document.createElement('div');
    document.body.appendChild(el);
    const app = createApp(() => h(PathMenu, { path: '/src/shop', stackId: 'ab12', canOpen: true }));
    app.use(createPinia());
    app.mount(el);
    await settle();
    el.querySelector<HTMLElement>('[data-testid="path-menu"]')!.click();
    await settle();
    expect(el.querySelector('[data-testid="path-menu-list"]')).not.toBeNull();
    el.querySelector<HTMLElement>('[data-testid="path-favorite"]')!.click();
    await settle();
    expect(fake.requests.find((r) => r.path === '/hub/open')?.body).toEqual({ stack: 'ab12', path: '/src/shop', with: 'vscode' });
    expect(el.querySelector('[data-testid="path-menu-list"]')).toBeNull();
    app.unmount();
  });
});
