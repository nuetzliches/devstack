// A shortcut card (`shortcut = true` in the stack file): its open button
// links to the service's own address, and a service with nothing to open
// shows no button.
import { describe, expect, it } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import { createPinia, setActivePinia } from 'pinia';
import type { ServiceView } from '@/api/types';
import ShortcutCard from './ShortcutCard.vue';

const service = (over: Partial<ServiceView>): ServiceView =>
  ({
    name: 'spa',
    kind: 'devserver',
    groups: [],
    port: 4500,
    url: 'http://localhost:4500',
    autostart: true,
    status: { state: 'healthy' },
    build_id: null,
    pid: 4145,
    since: 0,
    log: null,
    instance_port: null,
    proxy: null,
    candidate: null,
    public_link_available: true,
    shortcut: true,
    ...over,
  }) as ServiceView;

async function mountCard(s: ServiceView) {
  const pinia = createPinia();
  setActivePinia(pinia);
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp(() => h(ShortcutCard, { service: s }));
  app.use(pinia);
  app.mount(el);
  await nextTick();
  return { el, app };
}

describe('ShortcutCard', () => {
  it('opens the service address from a large button', async () => {
    const { el, app } = await mountCard(service({}));
    const open = el.querySelector('[data-testid="shortcut-open"]') as HTMLAnchorElement | null;
    expect(open).not.toBeNull();
    expect(open!.getAttribute('href')).toBe('http://localhost:4500');
    expect(open!.target).toBe('_blank');
    expect(el.querySelector('[data-testid="shortcut-spa"]')?.getAttribute('data-state')).toBe('healthy');
    app.unmount();
  });

  it('shows no button for a service with no port and no link', async () => {
    const { el, app } = await mountCard(service({ port: null, url: null, link: undefined }));
    expect(el.querySelector('[data-testid="shortcut-open"]')).toBeNull();
    expect(el.textContent).toContain('No port or link to open');
    app.unmount();
  });
});
