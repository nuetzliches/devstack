// The Skills page's flow (#482): hooks as nodes with their tooltip, a hook
// that is not given greyed with why.
import { describe, expect, it } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import type { PoolSkillsState } from '@/api/types';
import { mockActivity, mockHookDetails } from '@/api/mock/activity';
import SkillsFlow from './SkillsFlow.vue';

const skill = (name: string) => ({ name, description: '', sha256: name, installed: true, pending: false, enabled: true, hooks: mockHookDetails[name].length, hook_details: mockHookDetails[name] });
const state: PoolSkillsState = {
  agents: [],
  sets: [{ pool: 'default', set: 'core', source: 'x', state: 'accepted', auto: true, skills: [skill('work-session'), skill('work-issue'), skill('session-end')] }],
  conflicts: [],
};

async function mount(s: PoolSkillsState = state) {
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp({ render: () => h(SkillsFlow, { state: s, activity: mockActivity(), now: Date.now() }) });
  app.mount(el);
  await nextTick();
  return { el, unmount: () => (app.unmount(), el.remove()) };
}

describe('the flow tab', () => {
  it('shows each hook in its phase, with a tooltip that says when, on what condition and how often', async () => {
    const { el, unmount } = await mount();
    const phases = [...el.querySelectorAll<HTMLElement>('[data-testid="skills-flow-phase"]')].map((p) => p.dataset.phase);
    expect(phases).toEqual(['start', 'prompt', 'lane-stop', 'end']);
    const start = el.querySelector<HTMLElement>('[data-key="default/core/work-session#0"]')!;
    const tip = start.querySelector('[data-testid="skills-flow-tip"]')!.textContent!;
    expect(tip).toContain('at the start of a session');
    expect(tip).toContain('the upstream timetrack is there');
    expect(tip).toContain('once a session');
    expect(start.querySelector('[data-testid="skills-flow-fired"]')!.textContent).toContain('given');
    unmount();
  });

  it('greys a hook that is not given and says why', async () => {
    const { el, unmount } = await mount();
    const end = el.querySelector<HTMLElement>('[data-key="default/core/session-end#1"]')!;
    expect(end.classList.contains('off')).toBe(true);
    expect(end.querySelector('[data-testid="skills-flow-tip"]')!.textContent).toContain('neither accepted nor set up');
    unmount();
  });

  it('says so when no skill switched on carries hooks', async () => {
    const { el, unmount } = await mount({ ...state, sets: [] });
    expect(el.querySelector('[data-testid="skills-flow-empty"]')!.textContent).toContain('No skill switched on carries hooks');
    unmount();
  });
});
