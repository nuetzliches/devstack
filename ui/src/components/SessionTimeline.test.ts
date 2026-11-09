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
    // Each kind of token on its own row and scale: the output reaches the top of
    // its row, however much more is read from the cache.
    const tops = ['output', 'cache_read'].map((k) => {
      const d = el.querySelector(`[data-testid="session-timeline-tokens-${k}"] .line`)!.getAttribute('d')!;
      const ys = [...d.matchAll(/V([\d.]+)/g)].map((m) => Number(m[1]));
      return { min: Math.min(...ys), max: Math.max(...ys) };
    });
    for (const t of tops) expect(t.max - t.min).toBeGreaterThan(20);
    // The wait between the prompts is pushed together.
    expect(el.querySelectorAll('[data-testid="session-timeline-gap"]').length).toBeGreaterThan(0);
    unmount();
  });

  it('puts the tooltip on the page, outside the plot that clips what it holds', async () => {
    mockTranscripts.enabled = true;
    const { el, unmount } = await mount('b7c2');
    const call = el.querySelector('[data-testid="session-timeline-calls-devstack"] g')!;
    call.dispatchEvent(new MouseEvent('mouseenter', { clientX: 100, clientY: 100 }));
    await nextTick();
    const tip = document.body.querySelector<HTMLElement>('[data-testid="session-timeline-tip"]')!;
    expect(tip).not.toBeNull();
    expect(el.contains(tip)).toBe(false);
    expect(tip.style.left).toBe('114px');
    unmount();
    expect(document.body.querySelector('[data-testid="session-timeline-tip"]')).toBeNull();
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
