// A program's own icon from the hub, else its kind's: the mock hub and a
// program the hub has no icon for show the kind's, and a failed icon is not
// asked for again on the page.
import { describe, expect, it, vi } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import type { Opener } from '@/api/types';

const t = vi.hoisted(() => ({ kind: 'real' as 'real' | 'mock' }));

vi.mock('@/api', async (importOriginal) => {
  const real = await importOriginal<typeof import('@/api')>();
  return { ...real, api: () => ({ kind: t.kind }) };
});

const OpenerIcon = (await import('./OpenerIcon.vue')).default;

function mount(opener: Opener) {
  const el = document.createElement('div');
  const app = createApp(() => h(OpenerIcon, { opener }));
  app.mount(el);
  return { el, app };
}

describe('OpenerIcon', () => {
  it("shows the hub's icon of the program", () => {
    t.kind = 'real';
    const { el, app } = mount({ id: 'vscode', label: 'VS Code', kind: 'editor' });
    const img = el.querySelector('img');
    expect(img?.getAttribute('src')).toBe('/hub/openers/vscode/icon');
    expect(img?.getAttribute('alt')).toBe('');
    app.unmount();
  });

  it("falls back to the kind's icon, and remembers that", async () => {
    t.kind = 'real';
    const first = mount({ id: 'orca', label: 'Orca', kind: 'agent' });
    first.el.querySelector('img')!.dispatchEvent(new Event('error'));
    await nextTick();
    expect(first.el.querySelector('img')).toBeNull();
    expect(first.el.querySelector('svg')).not.toBeNull();
    const second = mount({ id: 'orca', label: 'Orca', kind: 'agent' });
    expect(second.el.querySelector('img')).toBeNull();
    first.app.unmount();
    second.app.unmount();
  });

  it("shows the kind's icon against the mock hub", () => {
    t.kind = 'mock';
    const { el, app } = mount({ id: 'zed', label: 'Zed', kind: 'editor' });
    expect(el.querySelector('img')).toBeNull();
    expect(el.querySelector('svg')).not.toBeNull();
    app.unmount();
  });
});
