// The badge of a branch ahead of and behind its upstream: ahead and not
// behind, a click asks and then pushes; behind it, there is nothing to click.
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

const { useUi } = await import('@/stores/ui');
const SyncBadge = (await import('./SyncBadge.vue')).default;

let fake: FakeTransport;

beforeEach(() => {
  setActivePinia(createPinia());
  fake = new FakeTransport();
  t.current = fake;
});

async function settle() {
  for (let i = 0; i < 5; i++) {
    await new Promise((r) => setTimeout(r, 0));
    await nextTick();
  }
}

function mount(props: Record<string, unknown>) {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp(() => h(SyncBadge, { stackId: 'ab12', path: '/src/shop', branch: 'main', upstream: 'origin/main', ahead: 1, behind: 0, ...props }));
  const pinia = createPinia();
  setActivePinia(pinia);
  app.use(pinia);
  app.mount(el);
  return { el, app, ui: useUi() };
}

describe('SyncBadge', () => {
  it('shows ↑n and ↓m, only the parts that are not zero', () => {
    const { el, app } = mount({ ahead: 2, behind: 1 });
    expect(el.querySelector('[data-testid="branch-ahead"]')?.textContent).toBe('↑2');
    expect(el.querySelector('[data-testid="branch-behind"]')?.textContent).toBe('↓1');
    app.unmount();
    const b = mount({ ahead: 0, behind: 3 });
    expect(b.el.querySelector('[data-testid="branch-ahead"]')).toBeNull();
    expect(b.el.querySelector('[data-testid="branch-behind"]')?.textContent).toBe('↓3');
    b.app.unmount();
  });

  it('is no button when the branch is behind its upstream, or the tree is not ours', () => {
    for (const props of [{ ahead: 2, behind: 1 }, { ahead: 0, behind: 3 }, { ahead: 1, behind: 0, readOnly: true }]) {
      const { el, app } = mount(props);
      expect(el.querySelector('button')).toBeNull();
      app.unmount();
    }
  });

  it('asks first, then asks the hub to push the tree it names', async () => {
    fake.on('/hub/stacks/ab12/push', ok({}));
    const { el, app, ui } = mount({ ahead: 2 });
    const button = el.querySelector<HTMLButtonElement>('button[data-testid="branch-sync"]')!;
    expect(button.title).toContain('2 commits not on origin/main');
    expect(button.title).toContain('Push main to origin/main');
    button.click();
    await vi.waitFor(() => expect(ui.confirmRequest).not.toBeNull());
    expect(ui.confirmRequest!.text).toBe('Push 2 commits of main to origin/main?');
    expect(fake.requests.filter((r) => r.path === '/hub/stacks/ab12/push')).toHaveLength(0);
    ui.confirmRequest!.resolve(true);
    await settle();
    const sent = fake.requests.filter((r) => r.path === '/hub/stacks/ab12/push');
    expect(sent).toHaveLength(1);
    expect(sent[0].method).toBe('POST');
    expect((sent[0].body as { path: string }).path).toBe('/src/shop');
    app.unmount();
  });

  it('pushes nothing when the question is declined', async () => {
    fake.on('/hub/stacks/ab12/push', ok({}));
    const { el, app, ui } = mount({});
    el.querySelector<HTMLButtonElement>('button')!.click();
    await vi.waitFor(() => expect(ui.confirmRequest).not.toBeNull());
    ui.confirmRequest!.resolve(false);
    await settle();
    expect(fake.requests.filter((r) => r.path === '/hub/stacks/ab12/push')).toHaveLength(0);
    // And it can be asked again.
    expect(el.querySelector<HTMLButtonElement>('button')!.disabled).toBe(false);
    app.unmount();
  });

  it("shows git's answer when the hub refuses, and the button is free again", async () => {
    fake.on('/hub/stacks/ab12/push', { status: 409, ok: false, body: { error: 'error: failed to push some refs' } });
    const { el, app, ui } = mount({});
    el.querySelector<HTMLButtonElement>('button')!.click();
    await vi.waitFor(() => expect(ui.confirmRequest).not.toBeNull());
    ui.confirmRequest!.resolve(true);
    await vi.waitFor(() => expect(ui.problem).not.toBeNull());
    expect(ui.problem!.title).toBe('Push main was refused');
    expect(ui.problem!.message).toContain('failed to push');
    await settle();
    expect(el.querySelector<HTMLButtonElement>('button')!.disabled).toBe(false);
    app.unmount();
  });
});
