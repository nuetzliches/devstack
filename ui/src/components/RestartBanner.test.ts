// The restart banner (a changed stack file, another devstack installed): the
// same one above the stack's page and a lane's views, with its Reload
// button.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h, nextTick, ref } from 'vue';
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

const { useHub } = await import('@/stores/hub');
const { useStack } = await import('@/stores/stack');
const RestartBanner = (await import('./RestartBanner.vue')).default;

const entry = (version: string): StackEntry => ({
  id: 'shop',
  name: 'shop',
  file: '/src/shop/devstack.toml',
  repo: '/src/shop',
  running: true,
  daemon: { pid: 1, port: 7000, started: 0, version },
  last_seen: null,
});

beforeEach(() => {
  localStorage.clear();
  document.body.innerHTML = '';
  setActivePinia(createPinia());
  const fake = new FakeTransport();
  t.current = fake;
  fake.on('/hub/session', ok({ version: '0.9.8' }));
});

/** The banner as a view mounts it: the daemon runs `daemonVersion`, the hub's is 0.9.8; `lane` is the one shown. */
async function mounted(daemonVersion: string, lane: string | null = null) {
  const hub = useHub();
  await hub.init();
  (t.current as FakeTransport).stream('/hub/events').handlers.onMessage({ type: 'stacks', stacks: [entry(daemonVersion)] });
  useStack().select('shop', { lane, remember: false });
  const configChanged = ref(false);
  const el = document.createElement('div');
  document.body.appendChild(el);
  createApp({ render: () => h(RestartBanner, { configChanged: configChanged.value }) }).mount(el);
  await nextTick();
  return { el, configChanged };
}

describe('the restart banner', () => {
  it('says nothing while neither the stack file nor the devstack version differs', async () => {
    const { el } = await mounted('0.9.8');
    expect(el.querySelector('.banner')).toBeNull();
  });

  it.each([null, 'feat'])('says the stack file changed, with the restart button, in the lane %s', async (lane) => {
    const { el, configChanged } = await mounted('0.9.8', lane);
    configChanged.value = true;
    await nextTick();
    expect(el.querySelector('[data-testid="config-changed"]')?.textContent).toContain('The stack file changed since shop started.');
    expect(el.querySelector('[data-testid="banner-restart-stack"]')?.textContent).toContain('Reload');
  });

  it('says another devstack is installed, and that the changed stack file applies with it', async () => {
    const { el, configChanged } = await mounted('0.9.7');
    expect(el.querySelector('[data-testid="daemon-outdated"]')?.textContent).toContain('shop runs devstack 0.9.7, devstack 0.9.8 is installed.');
    expect(el.textContent).not.toContain('applies the changed stack file');
    configChanged.value = true;
    await nextTick();
    expect(el.querySelector('[data-testid="config-changed"]')).toBeNull();
    expect(el.textContent).toContain('and applies the changed stack file');
  });
});
