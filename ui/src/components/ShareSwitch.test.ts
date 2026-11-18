// The share switch (ADR-0113): what the node agent's state says, the checks
// with their commands, and the switch sent with the form's distribution.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import { createPinia, setActivePinia } from 'pinia';
import type { ShareStatus } from '@/api/types';
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

const { usePool } = await import('@/stores/pool');
const ShareSwitch = (await import('./ShareSwitch.vue')).default;

const status = (over: Partial<ShareStatus>): ShareStatus => ({
  total: { cpus: 16, memory: 64 * 2 ** 30 },
  free: { cpus: 8, memory: 32 * 2 ** 30 },
  member: true,
  reported_at: null,
  error: null,
  ...over,
});

beforeEach(() => {
  document.body.innerHTML = '';
  setActivePinia(createPinia());
  t.current = new FakeTransport();
});

async function mounted(s: ShareStatus) {
  usePool().share = s;
  const el = document.createElement('div');
  document.body.appendChild(el);
  createApp({ render: () => h(ShareSwitch) }).mount(el);
  await nextTick();
  return el;
}

describe('the share switch', () => {
  it('shows what the distribution lacks, with the command for each', async () => {
    const el = await mounted(
      status({
        switch: { enabled: true, distro: 'Ubuntu' },
        host: {
          state: 'blocked',
          detail: 'The WSL distribution `Ubuntu` lacks what the node agent needs: see the checks.',
          guests: 0,
          distros: ['Ubuntu', 'Ubuntu-24.04'],
          checks: [
            { name: 'networking', ok: true, detail: 'networking mode: mirrored' },
            { name: 'docker', ok: false, detail: '`docker` talks to Docker Desktop', fix: 'curl -fsSL https://get.docker.com | sh' },
            { name: 'firewall', ok: false, detail: 'no Hyper-V firewall rule', fix: 'New-NetFirewallHyperVRule …', optional: true },
          ],
          prompt: 'Set up a WSL2 distribution …',
        },
      }),
    );
    expect(el.querySelector('[data-testid="share-state"]')?.textContent).toContain('Not ready yet');
    const checks = el.querySelector('[data-testid="share-checks"]')!;
    expect(checks.querySelectorAll('li.warn')).toHaveLength(1);
    expect(checks.textContent).toContain('curl -fsSL https://get.docker.com | sh');
    expect(el.querySelector('[data-testid="share-prompt"]')).not.toBeNull();
    const select = el.querySelector<HTMLSelectElement>('[data-testid="share-distro"]')!;
    expect([...select.options].map((o) => o.value)).toEqual(['', 'Ubuntu', 'Ubuntu-24.04']);
    expect(select.value).toBe('Ubuntu');
  });

  it('says that others still run lanes while it drains', async () => {
    const el = await mounted(status({ switch: { enabled: false }, host: { state: 'draining', guests: 2 } }));
    expect(el.querySelector('[data-testid="share-state"]')?.textContent).toBe('No new lanes; 2 of others still run here');
    expect(el.querySelector('[data-testid="share-distro"]')).toBeNull();
  });

  it('waits for an admin while the machine is not granted', async () => {
    const el = await mounted(status({ switch: { enabled: true }, host: { state: 'running', granted: false, guests: 0 } }));
    expect(el.querySelector('[data-testid="share-state"]')?.textContent).toContain('Waiting for an admin');
  });

  it('keeps what is typed when the same switch is read again', async () => {
    const el = await mounted(status({ switch: { enabled: true, max_cpus: 4 }, host: { state: 'running', guests: 0 } }));
    const cpus = el.querySelector<HTMLInputElement>('[data-testid="share-cap-cpus"]')!;
    cpus.value = '6';
    cpus.dispatchEvent(new Event('input'));
    // The poll: the same values in a new object.
    usePool().share = status({ switch: { enabled: true, max_cpus: 4 }, host: { state: 'running', guests: 0 } });
    await nextTick();
    expect(cpus.value).toBe('6');
    // Saved elsewhere: the form follows.
    usePool().share = status({ switch: { enabled: true, max_cpus: 2 }, host: { state: 'running', guests: 0 } });
    await nextTick();
    expect(cpus.value).toBe('2');
  });

  it('sends the switch with the chosen distribution', async () => {
    const el = await mounted(status({ switch: { enabled: false }, host: { state: 'off', guests: 0, distros: ['Ubuntu-24.04'] } }));
    const fake = t.current!;
    let sent: unknown = null;
    fake.on('/hub/pool/share', (method, _path, body) => {
      if (method === 'PUT') sent = body;
      return ok(status({ switch: { enabled: true, distro: 'Ubuntu-24.04' }, host: { state: 'starting', guests: 0 } }));
    });
    const select = el.querySelector<HTMLSelectElement>('[data-testid="share-distro"]')!;
    select.value = 'Ubuntu-24.04';
    select.dispatchEvent(new Event('change'));
    el.querySelector<HTMLButtonElement>('[data-testid="share-switch"]')!.click();
    await vi.waitFor(() => expect(sent).toEqual({ enabled: true, distro: 'Ubuntu-24.04' }));
  });
});
