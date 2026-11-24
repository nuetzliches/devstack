// "Add a stack" for a repository not cloned yet: the path menu's Git clients
// start to clone it, an https address pasted into the field goes to their
// clone dialog, and an address is never checked as a folder.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import { createPinia, setActivePinia } from 'pinia';
import { createMemoryHistory, createRouter } from 'vue-router';
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
const AddStackDialog = (await import('./AddStackDialog.vue')).default;

let fake: FakeTransport;

beforeEach(() => {
  localStorage.clear();
  document.body.innerHTML = '';
  setActivePinia(createPinia());
  fake = new FakeTransport();
  t.current = fake;
  fake.on('/hub/openers', ok([
    { id: 'vscode', label: 'VS Code', kind: 'editor' },
    { id: 'github-desktop', label: 'GitHub Desktop', kind: 'git' },
  ]));
  fake.on('/hub/clone', { status: 204, ok: true, body: null });
  useOpenerPrefs().save({ order: [], hidden: [], favorites: [] });
});

async function settle() {
  for (let i = 0; i < 5; i++) {
    await new Promise((r) => setTimeout(r, 0));
    await nextTick();
  }
}

function mount() {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp(() => h(AddStackDialog, { open: true }));
  app.use(createPinia());
  app.use(createRouter({ history: createMemoryHistory(), routes: [{ path: '/', component: { render: () => null } }] }));
  app.mount(el);
  return app;
}

const q = (id: string) => document.body.querySelectorAll<HTMLElement>(`[data-testid="${id}"]`);

async function type(value: string) {
  const input = document.body.querySelector<HTMLInputElement>('[data-testid="add-stack-path"]')!;
  input.value = value;
  input.dispatchEvent(new Event('input'));
  await settle();
}

describe('AddStackDialog, cloning', () => {
  it('offers the Git clients only, and starts one without an address', async () => {
    const app = mount();
    await settle();
    const buttons = q('clone-with-open');
    expect([...buttons].map((b) => b.dataset.id)).toEqual(['github-desktop']);
    buttons[0].click();
    await settle();
    expect(fake.requests.find((r) => r.path === '/hub/clone')?.body).toEqual({ with: 'github-desktop' });
    app.unmount();
  });

  it('hands a pasted https address to the clone dialog and does not check it as a folder', async () => {
    const app = mount();
    await settle();
    await type('https://github.com/acme/shop.git');
    await new Promise((r) => setTimeout(r, 500));
    expect(fake.requests.some((r) => r.path.startsWith('/hub/inspect'))).toBe(false);
    q('clone-with-open')[0].click();
    await settle();
    expect(fake.requests.find((r) => r.path === '/hub/clone')?.body).toEqual({ with: 'github-desktop', url: 'https://github.com/acme/shop.git' });
    app.unmount();
  });

  it('says that an ssh address is not handed over', async () => {
    const app = mount();
    await settle();
    await type('git@github.com:acme/shop.git');
    expect(q('add-stack-not-https')).toHaveLength(1);
    q('clone-with-open')[0].click();
    await settle();
    expect(fake.requests.find((r) => r.path === '/hub/clone')?.body).toEqual({ with: 'github-desktop' });
    app.unmount();
  });

  it('keeps the way to the programs when the Git client is unchecked', async () => {
    useOpenerPrefs().save({ order: [], hidden: ['github-desktop'], favorites: [] });
    const app = mount();
    await settle();
    expect(q('clone-with-open')).toHaveLength(0);
    expect(q('clone-with-manage')).toHaveLength(1);
    app.unmount();
  });

  it('shows nothing where no Git client is found', async () => {
    fake = new FakeTransport();
    t.current = fake;
    fake.on('/hub/openers', ok([{ id: 'vscode', label: 'VS Code', kind: 'editor' }]));
    const app = mount();
    await settle();
    expect(q('clone-with')).toHaveLength(0);
    app.unmount();
  });
});
