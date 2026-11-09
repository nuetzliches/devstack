// A session in time: its rows, the pause pushed together, the tokens; a
// session without its transcript shows devstack's records only.
import { describe, expect, it } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import { mockTimeline, mockTranscripts } from '@/api/mock/timeline';
import SessionTimeline from './SessionTimeline.vue';

async function mount(session: string) {
  const timeline = mockTimeline(session)!;
  const el = document.createElement('div');
  document.body.appendChild(el);
  const app = createApp({ render: () => h(SessionTimeline, { timeline, lane: [{ at: timeline.started + 60_000, title: 'api: restart', text: 'via the MCP server', tone: 'accent' }], now: Date.now() }) });
  app.mount(el);
  await nextTick();
  return { el, timeline, unmount: () => (app.unmount(), el.remove()) };
}

describe('the session timeline', () => {
  it('shows prompts, calls by where they went, and the tokens', async () => {
    mockTranscripts.enabled = true;
    const { el, timeline, unmount } = await mount('b7c2');
    expect(timeline.source).toBe('transcript');
    expect(el.querySelectorAll('[data-testid="session-timeline-prompts"] .prompt').length).toBe(timeline.marks.filter((m) => m.kind === 'prompt').length);
    expect(el.querySelector('[data-testid="session-timeline-calls-devstack"]')).not.toBeNull();
    expect(el.querySelector('[data-testid="session-timeline-calls-builtin"]')).not.toBeNull();
    expect(el.querySelector('[data-testid="session-timeline-tokens"]')).not.toBeNull();
    expect(el.querySelector('[data-testid="session-timeline-total"]')!.textContent).toMatch(/Tokens\s+\d/);
    // The wait between the prompts is pushed together.
    expect(el.querySelectorAll('[data-testid="session-timeline-gap"]').length).toBeGreaterThan(0);
    unmount();
  });

  it('shows only what devstack recorded while transcripts are not read', async () => {
    mockTranscripts.enabled = false;
    const { el, timeline, unmount } = await mount('b7c2');
    expect(timeline.why).toBe('off');
    expect(el.querySelector('[data-testid="session-timeline-calls-builtin"]')).toBeNull();
    expect(el.querySelector('[data-testid="session-timeline-calls-devstack"]')).not.toBeNull();
    expect(el.querySelector('[data-testid="session-timeline-tokens"]')).toBeNull();
    unmount();
    mockTranscripts.enabled = true;
  });
});
