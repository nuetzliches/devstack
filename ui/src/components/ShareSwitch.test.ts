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
            { name: 'docker', ok: false, detail: '`docker` talks to Docker Desktop', fix: 'curl -fsSL https://get.docker.com | sh', fix_note: 'In `Ubuntu` as root' },
            { name: 'firewall', ok: false, detail: 'no Hyper-V firewall rule', fix: 'New-NetFirewallHyperVRule …', optional: true },
          ],
          prompt: 'Set up a WSL2 distribution …',
        },
      }),
    );
    expect(el.querySelector('[data-testid="share-state"]')?.textContent).toContain('Setup incomplete');
    expect(el.querySelector('[data-testid="share"]')?.classList).toContain('tone-warn');
    const checks = el.querySelector('[data-testid="share-checks"]')!;
    expect(checks.querySelectorAll('li.warn')).toHaveLength(1);
    // The command alone is copied; where it runs stands above it.
    expect(checks.querySelector('[data-testid="share-fix"]')?.textContent).toBe('curl -fsSL https://get.docker.com | sh');
    expect(checks.querySelector('[data-testid="share-fix-note"]')?.textContent).toBe('In `Ubuntu` as root:');
    expect(el.querySelector('[data-testid="share-prompt"]')).not.toBeNull();
    // The state and the button open the setup dialog.
    el.querySelector<HTMLButtonElement>('[data-testid="share-finish"]')!.click();
    expect(usePool().shareSetup).toBe(true);
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

  it('is green only while the agent runs and the machine is granted', async () => {
    const el = await mounted(status({ switch: { enabled: true }, host: { state: 'running', granted: true, guests: 0 } }));
    expect(el.querySelector('[data-testid="share"]')?.classList).toContain('tone-ok');
    usePool().share = status({ switch: { enabled: true }, host: { state: 'running', granted: false, guests: 0 } });
    await nextTick();
    expect(el.querySelector('[data-testid="share"]')?.classList).not.toContain('tone-ok');
  });

  it('switched on, opens the setup instead of sending the switch', async () => {
    const el = await mounted(status({ switch: { enabled: false }, host: { state: 'off', guests: 0, distros: ['Ubuntu-24.04'] } }));
    el.querySelector<HTMLButtonElement>('[data-testid="share-switch"]')!.click();
    await nextTick();
    expect(usePool().shareSetup).toBe(true);
    expect(t.current!.requests.filter((r) => r.method === 'PUT')).toHaveLength(0);
  });
});

describe('the share setup dialog', () => {
  it('checks before the switch goes on, then sends it with the chosen distribution', async () => {
    const fake = t.current!;
    const checked: unknown[] = [];
    fake.on('/hub/pool/share/check', (_m, _p, body) => {
      checked.push(body);
      const d = (body as { distro?: string }).distro;
      return ok(
        d === 'Ubuntu-24.04'
          ? { ready: true, distros: ['Ubuntu', 'Ubuntu-24.04'], checks: [{ name: 'docker', ok: true, detail: 'Docker Engine' }] }
          : { ready: false, problem: 'Choose the WSL distribution', distros: ['Ubuntu', 'Ubuntu-24.04'] },
      );
    });
    let sent: unknown = null;
    fake.on('/hub/pool/share', (method, _path, body) => {
      if (method === 'PUT') sent = body;
      return ok(status({ switch: { enabled: true, distro: 'Ubuntu-24.04' }, host: { state: 'starting', guests: 0 } }));
    });
    usePool().share = status({ switch: { enabled: false }, host: { state: 'off', guests: 0 } });
    const Dialog = (await import('./ShareSetupDialog.vue')).default;
    const el = document.createElement('div');
    document.body.appendChild(el);
    createApp({ render: () => h(Dialog) }).mount(el);
    usePool().shareSetup = true;
    await vi.waitFor(() => expect(checked).toHaveLength(1));
    const q = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`);
    expect(q('share-setup-about')).not.toBeNull();
    q('share-setup-next')!.click();
    await vi.waitFor(() => expect(q('share-setup-problem')?.textContent).toContain('Choose'));
    expect((q('share-setup-next') as HTMLButtonElement).disabled).toBe(true);

    const select = q('share-setup-distro') as HTMLSelectElement;
    select.value = 'Ubuntu-24.04';
    select.dispatchEvent(new Event('change'));
    await vi.waitFor(() => expect(q('share-setup-ready')).not.toBeNull());
    expect(sent).toBeNull();
    q('share-setup-next')!.click();
    await nextTick();
    q('share-setup-go')!.click();
    await vi.waitFor(() => expect(sent).toEqual({ enabled: true, distro: 'Ubuntu-24.04' }));
    await vi.waitFor(() => expect(q('share-setup-outcome')).not.toBeNull());
  });

  // Code review before 0.51.0, finding 7.
  it('shows the check of the distribution chosen last, not a slower earlier one', async () => {
    const fake = t.current!;
    const later: Array<() => void> = [];
    fake.on('/hub/pool/share/check', (_m, _p, body) => {
      const d = (body as { distro?: string }).distro;
      const answer = ok(
        d === 'Ubuntu-24.04'
          ? { ready: false, problem: 'Docker is missing in Ubuntu-24.04', distros: ['Ubuntu', 'Ubuntu-24.04'] }
          : { ready: true, distros: ['Ubuntu', 'Ubuntu-24.04'], checks: [{ name: 'docker', ok: true, detail: 'Docker Engine' }] },
      );
      // The check of `Ubuntu` answers only when let go.
      return d === 'Ubuntu' ? new Promise((done) => later.push(() => done(answer))) : answer;
    });
    usePool().share = status({ switch: { enabled: true, distro: 'Ubuntu-24.04' }, host: { state: 'blocked', guests: 0 } });
    const Dialog = (await import('./ShareSetupDialog.vue')).default;
    const el = document.createElement('div');
    document.body.appendChild(el);
    createApp({ render: () => h(Dialog) }).mount(el);
    usePool().shareSetup = true;
    const q = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`);
    await vi.waitFor(() => expect(q('share-setup-problem')?.textContent).toContain('Ubuntu-24.04'));
    const select = q('share-setup-distro') as HTMLSelectElement;
    select.value = 'Ubuntu';
    select.dispatchEvent(new Event('change'));
    await vi.waitFor(() => expect(later).toHaveLength(1));
    // While `Ubuntu` is checked, the other's verdict is not shown as its.
    expect(q('share-setup-problem')).toBeNull();
    select.value = 'Ubuntu-24.04';
    select.dispatchEvent(new Event('change'));
    await vi.waitFor(() => expect(q('share-setup-problem')?.textContent).toContain('Ubuntu-24.04'));
    later[0]();
    await new Promise((r) => setTimeout(r, 20));
    expect(q('share-setup-ready')).toBeNull();
    expect(q('share-setup-problem')?.textContent).toContain('Ubuntu-24.04');
  });
});
