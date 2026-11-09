<script setup lang="ts">
// A session of a coding agent in time: the prompts (when, never what), the
// model's turns, the tool calls by where they went, what changed the lane,
// the hooks devstack gave; below, how full the context was and the tokens
// summed up. A pause longer than two minutes is pushed together. Hover a
// mark for its details; the crosshair reads the charts at its time.
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import type { SessionTimeline, TimelineCall, TimelineCallKind } from '@/api/types';
import { duration } from '@/lib/format';
import type { Tone } from '@/lib/status';
import { axis, busySpans, context, cumulative, pack, ROWS, sumAt, tickLabel, tokens, total, turnOf } from '@/lib/timeline';

export type LaneMark = { at: number; title: string; text: string; tone: Tone };

const props = defineProps<{ timeline: SessionTimeline; lane: LaneMark[]; now: number }>();

const LABEL_W = 96;
const AXIS_H = 20;
const BAR_H = 8;
const CTX_H = 56;
const SUM_H = 72;

// The width of the plot follows the card; the zoom widens it (it scrolls then).
const box = ref<HTMLElement | null>(null);
const boxW = ref(800);
const zoom = ref(1);
let ro: ResizeObserver | null = null;
onMounted(() => {
  if (!box.value) return;
  ro = new ResizeObserver(([e]) => (boxW.value = Math.max(320, e.contentRect.width)));
  ro.observe(box.value);
});
onBeforeUnmount(() => ro?.disconnect());

const t = computed(() => props.timeline);
const W = computed(() => Math.round((boxW.value - 2) * zoom.value));
const ax = computed(() => axis(busySpans(t.value, props.lane.map((m) => m.at).filter((at) => at >= t.value.started - 60_000 && at <= t.value.last + 60_000), props.now), W.value));
const lane = computed(() => props.lane.filter((m) => m.at >= ax.value.segments[0].from && m.at <= ax.value.segments[ax.value.segments.length - 1].to));
/** The ticks not under a gap's label. */
const ticks = computed(() => ax.value.ticks.filter((k) => ax.value.gaps.every((g) => k.x < g.x - 30 || k.x > g.x + 60)));
const withSeconds = computed(() => ax.value.ticks.length > 1 && ax.value.ticks[1].at - ax.value.ticks[0].at < 60_000);
const callEnd = (c: TimelineCall) => c.at + (c.ms ?? (t.value.ended ? 0 : Math.max(0, props.now - c.at)));

type Row = { id: string; label: string; y: number; h: number };
const layout = computed(() => {
  const rows: Row[] = [];
  let y = AXIS_H;
  const add = (id: string, label: string, h: number) => {
    rows.push({ id, label, y, h });
    y += h;
  };
  add('prompts', 'Prompts', 22);
  if (t.value.turns.length) add('turns', 'Model', 24);
  const calls: { kind: TimelineCallKind; row: Row; packed: ReturnType<typeof pack>; list: TimelineCall[] }[] = [];
  for (const r of ROWS) {
    const list = t.value.calls.filter((c) => c.kind === r.kind);
    if (!list.length) continue;
    const packed = pack(list, ax.value.x, callEnd);
    const lines = Math.min(packed.lines, 4);
    add(`calls-${r.kind}`, r.label, lines * (BAR_H + 3) + 9);
    calls.push({ kind: r.kind, row: rows[rows.length - 1], packed: { line: packed.line.map((l) => Math.min(l, 3)), lines }, list });
  }
  if (lane.value.length) add('lane', 'Lane', 22);
  if (t.value.hooks.length) add('hooks', 'Hooks', 22);
  const plotH = y;
  if (t.value.turns.length) {
    y += 10;
    add('context', 'Context', CTX_H);
    y += 6;
    add('sum', 'Tokens', SUM_H);
  }
  return { rows, calls, plotH, height: y + 4 };
});
const row = (id: string) => layout.value.rows.find((r) => r.id === id);

// The turns: a tick each, as tall as its output (square root, so a long one does not flatten the rest).
const turns = computed(() => {
  const r = row('turns');
  if (!r) return [];
  const max = Math.sqrt(Math.max(1, ...t.value.turns.map((u) => u.output)));
  return t.value.turns.map((u) => {
    const h = 3 + (Math.sqrt(u.output) / max) * (r.h - 8);
    return { u, x: ax.value.x(u.at), y: r.y + r.h - 3 - h, h };
  });
});

const sums = computed(() => cumulative(t.value.turns));
const ctx = computed(() => context(t.value.turns));
const ctxMax = computed(() => Math.max(1, ...ctx.value.map((c) => c.tokens)));

/** The tokens summed up, stacked: read from the cache at the bottom, then written to it, input, output. */
const LAYERS = [
  { key: 'cache_read', label: 'Cache read', cls: 'l-read' },
  { key: 'cache_write', label: 'Cache written', cls: 'l-write' },
  { key: 'input', label: 'Input', cls: 'l-input' },
  { key: 'output', label: 'Output', cls: 'l-output' },
] as const;
type LayerKey = (typeof LAYERS)[number]['key'];
/** Layers left out of the chart (a click on the legend): what is read from the cache dwarfs the rest, so it starts hidden. */
const hidden = ref<Set<LayerKey>>(new Set(['cache_read']));
function toggle(k: LayerKey) {
  const next = new Set(hidden.value);
  if (!next.delete(k)) next.add(k);
  hidden.value = next;
}
const layers = computed(() => LAYERS.filter((l) => !hidden.value.has(l.key)));
const sumMax = computed(() => Math.max(1, ...sums.value.map((s) => layers.value.reduce((n, l) => n + s[l.key], 0))));

/** A step line through the points, from the axis' start to its end (or now). */
function steps(pts: { at: number; v: number }[], max: number, r: Row, base = 0): string {
  if (!pts.length) return '';
  const y = (v: number) => r.y + r.h - (v / max) * (r.h - 4);
  let d = `M${ax.value.x(pts[0].at).toFixed(1)} ${y(base).toFixed(1)}`;
  for (const p of pts) d += ` H${ax.value.x(p.at).toFixed(1)} V${y(p.v).toFixed(1)}`;
  d += ` H${W.value}`;
  return d;
}
const ctxPath = computed(() => {
  const r = row('context');
  if (!r) return null;
  const line = steps(ctx.value.map((c) => ({ at: c.at, v: c.tokens })), ctxMax.value, r);
  return { line, area: `${line} V${r.y + r.h} H${ax.value.x(ctx.value[0]?.at ?? 0).toFixed(1)} Z` };
});

const sumPaths = computed(() => {
  const r = row('sum');
  if (!r || !sums.value.length) return [];
  const LS = layers.value;
  const x0 = ax.value.x(sums.value[0].at);
  const y = (v: number) => r.y + r.h - (v / sumMax.value) * (r.h - 4);
  return LS.map((l, i) => {
    const top = (s: (typeof sums.value)[number]) => LS.slice(0, i + 1).reduce((n, k) => n + s[k.key], 0);
    const bottom = (s: (typeof sums.value)[number]) => LS.slice(0, i).reduce((n, k) => n + s[k.key], 0);
    let up = `M${x0.toFixed(1)} ${y(0).toFixed(1)}`;
    for (const s of sums.value) up += ` H${ax.value.x(s.at).toFixed(1)} V${y(top(s)).toFixed(1)}`;
    up += ` H${W.value}`;
    // Back along the layer below.
    const last = sums.value[sums.value.length - 1];
    let down = ` V${y(bottom(last)).toFixed(1)}`;
    for (let k = sums.value.length - 1; k >= 0; k--) {
      const s = sums.value[k];
      down += ` H${ax.value.x(s.at).toFixed(1)} V${y(k > 0 ? bottom(sums.value[k - 1]) : 0).toFixed(1)}`;
    }
    return { ...l, d: `${up}${down} H${x0.toFixed(1)} Z` };
  });
});

const prompts = computed(() => t.value.marks.filter((m) => m.kind === 'prompt'));
const others = computed(() => t.value.marks.filter((m) => m.kind !== 'prompt'));
const markWords: Record<string, string> = {
  prompt: 'Prompt sent',
  interrupt: 'Interrupted by you',
  compact: 'Context compacted',
  clear: 'Context cleared',
  error: 'The model\'s API failed',
};

// Summary over the session.
const summary = computed(() => {
  const last = sums.value[sums.value.length - 1];
  const models = [...new Set(t.value.turns.filter((u) => !u.sidechain).map((u) => u.model).filter(Boolean))];
  const failed = t.value.calls.filter((c) => !c.ok).length;
  const busy = ax.value.segments.reduce((n, s) => n + (s.to - s.from), 0);
  return { last, models, failed, busy };
});

// --- the tooltip and the crosshair
type Tip = { x: number; y: number; title: string; lines: string[]; tone?: string };
const tip = ref<Tip | null>(null);
const cross = ref<number | null>(null);
const svg = ref<SVGSVGElement | null>(null);

function tokensLine(at: number): string {
  const s = sumAt(sums.value, at);
  return `until then ${tokens(total(s))} tokens (out ${tokens(s.output)})`;
}
function show(e: MouseEvent, title: string, lines: string[], tone?: string) {
  const r = box.value?.getBoundingClientRect();
  if (!r) return;
  tip.value = { x: e.clientX - r.left + (box.value?.scrollLeft ?? 0), y: e.clientY - r.top, title, lines, tone };
}
function callTip(e: MouseEvent, c: TimelineCall) {
  const u = turnOf(t.value.turns, c.at);
  const took = c.ms === null ? (t.value.ended ? 'no answer' : 'running') : c.ms < 1000 ? `${c.ms} ms` : duration(c.ms);
  const lines = [
    `${c.kind === 'builtin' ? 'built-in' : c.kind === 'agent' ? 'sub-agent' : c.kind}${c.upstream ? ` · ${c.upstream}` : ''} · ${took}`,
    `${new Date(c.at).toLocaleTimeString()}${c.ok ? '' : ' · failed'}`,
  ];
  if (t.value.turns.length) lines.push(tokensLine(c.at));
  if (u) lines.push(`asked by a turn: in ${tokens(u.input + u.cache_write + u.cache_read)} · out ${tokens(u.output)}`);
  show(e, c.tool, lines, c.ok ? undefined : 'danger');
}
function move(e: MouseEvent) {
  const r = svg.value?.getBoundingClientRect();
  if (!r) return;
  cross.value = e.clientX - r.left;
}
const crossInfo = computed(() => {
  if (cross.value === null || !t.value.turns.length) return null;
  const at = ax.value.at(cross.value);
  const s = sumAt(sums.value, at);
  let c = 0;
  for (const p of ctx.value) if (p.at <= at) c = p.tokens;
  return { at, s, c };
});
</script>

<template>
  <div class="tl" data-testid="session-timeline">
    <div class="sum-line small">
      <span v-if="summary.models.length"><span class="k">Model</span> {{ summary.models.join(', ') }}</span>
      <span><span class="k">Prompts</span> {{ prompts.length }}</span>
      <span><span class="k">Calls</span> {{ t.calls.length }}<template v-if="summary.failed"> · <span class="err">{{ summary.failed }} failed</span></template></span>
      <span><span class="k">Busy</span> {{ duration(summary.busy) }}</span>
      <span v-if="summary.last" data-testid="session-timeline-total">
        <span class="k">Tokens</span> {{ tokens(total(summary.last)) }}
        <span class="faint">(cache read {{ tokens(summary.last.cache_read) }} · written {{ tokens(summary.last.cache_write) }} · in {{ tokens(summary.last.input) }} · out {{ tokens(summary.last.output) }})</span>
      </span>
      <span class="grow" />
      <span class="zoom btn-group" role="group" aria-label="Zoom">
        <button v-for="z in [1, 2, 4, 8]" :key="z" type="button" class="chip-toggle" :aria-pressed="zoom === z" :data-testid="`session-timeline-zoom-${z}`" @click="zoom = z">{{ z }}×</button>
      </span>
    </div>

    <div class="frame">
      <div class="labels" :style="{ width: `${LABEL_W}px`, height: `${layout.height}px` }">
        <span v-for="r in layout.rows" :key="r.id" class="label small" :style="{ top: `${r.y}px`, height: `${r.h}px` }">
          {{ r.label }}
          <small v-if="r.id === 'context'" class="faint">max {{ tokens(ctxMax) }}</small>
          <small v-if="r.id === 'sum'" class="faint">{{ tokens(sumMax) }}{{ hidden.size ? ' shown' : '' }}</small>
        </span>
      </div>
      <div ref="box" class="plot" @mouseleave="(tip = null), (cross = null)">
        <svg ref="svg" :width="W" :height="layout.height" :viewBox="`0 0 ${W} ${layout.height}`" role="img" :aria-label="`Session ${t.session} in time`" @mousemove="move">
          <defs>
            <pattern id="tl-gap" width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
              <line x1="0" y1="0" x2="0" y2="6" class="gap-hatch" />
            </pattern>
          </defs>
          <!-- Rows -->
          <rect v-for="(r, i) in layout.rows" :key="`bg-${r.id}`" x="0" :y="r.y" :width="W" :height="r.h" :class="i % 2 ? 'row-b' : 'row-a'" />
          <!-- Ticks -->
          <g class="ticks">
            <g v-for="k in ticks" :key="k.at">
              <line :x1="k.x" :x2="k.x" :y1="AXIS_H - 4" :y2="layout.height" />
              <text :x="k.x + 3" :y="12">{{ tickLabel(k.at, withSeconds) }}</text>
            </g>
          </g>
          <!-- Pauses pushed together -->
          <g v-for="g in ax.gaps" :key="`gap-${g.x}`" class="gap" data-testid="session-timeline-gap">
            <rect :x="g.x + 2" :y="AXIS_H - 6" :width="24" :height="layout.height - AXIS_H + 6" fill="url(#tl-gap)" />
            <text :x="g.x + 14" :y="12" text-anchor="middle">⋯ {{ duration(g.ms) }}</text>
            <title>Nothing happened for {{ duration(g.ms) }}</title>
          </g>
          <!-- Compacted, cleared, interrupted, failed: a line through all rows -->
          <g v-for="(m, i) in others" :key="`m-${i}`" class="mark" :class="`m-${m.kind}`" @mouseenter="show($event, markWords[m.kind], [new Date(m.at).toLocaleTimeString()])" @mouseleave="tip = null">
            <line :x1="ax.x(m.at)" :x2="ax.x(m.at)" :y1="AXIS_H" :y2="layout.height" />
            <text :x="ax.x(m.at) + 3" :y="(row('prompts')?.y ?? 0) + 15">{{ m.kind === 'interrupt' ? '✕' : m.kind }}</text>
            <rect :x="ax.x(m.at) - 5" :y="AXIS_H" width="10" :height="layout.height - AXIS_H" class="hit" />
          </g>
          <!-- Prompts -->
          <g v-if="row('prompts')" data-testid="session-timeline-prompts">
            <g v-for="(m, i) in prompts" :key="`p-${i}`" class="prompt" @mouseenter="show($event, `Prompt #${i + 1}`, [new Date(m.at).toLocaleTimeString(), ...(t.turns.length ? [tokensLine(m.at)] : [])])" @mouseleave="tip = null">
              <path :d="`M${ax.x(m.at)} ${row('prompts')!.y + 5} l5 12 h-10 z`" />
              <text :x="ax.x(m.at) + 7" :y="row('prompts')!.y + 15">#{{ i + 1 }}</text>
              <rect :x="ax.x(m.at) - 6" :y="row('prompts')!.y" width="26" :height="row('prompts')!.h" class="hit" />
            </g>
          </g>
          <!-- Turns -->
          <g v-if="row('turns')" class="turns">
            <g
              v-for="(k, i) in turns"
              :key="`u-${i}`"
              @mouseenter="show($event, k.u.sidechain ? 'Sub-agent turn' : 'Turn', [`${k.u.model ?? 'model'} · ${new Date(k.u.at).toLocaleTimeString()}`, `context ${tokens(k.u.input + k.u.cache_write + k.u.cache_read)} (cache read ${tokens(k.u.cache_read)}, written ${tokens(k.u.cache_write)}, new ${tokens(k.u.input)})`, `output ${tokens(k.u.output)}`])"
              @mouseleave="tip = null"
            >
              <rect :x="k.x - 1" :y="k.y" width="2.5" :height="k.h" rx="1" :class="{ side: k.u.sidechain }" />
              <rect :x="k.x - 3" :y="row('turns')!.y" width="7" :height="row('turns')!.h" class="hit" />
            </g>
          </g>
          <!-- Calls -->
          <g v-for="g in layout.calls" :key="g.kind" :class="`calls k-${g.kind}`" :data-testid="`session-timeline-calls-${g.kind}`">
            <g v-for="(c, i) in g.list" :key="`c-${i}`" :class="{ failed: !c.ok, open: c.ms === null }" @mouseenter="callTip($event, c)" @mouseleave="tip = null">
              <rect :x="ax.x(c.at)" :y="g.row.y + 5 + g.packed.line[i] * (BAR_H + 3)" :width="Math.max(3, ax.x(callEnd(c)) - ax.x(c.at))" :height="BAR_H" rx="2" class="bar" />
              <rect :x="ax.x(c.at) - 3" :y="g.row.y + 3 + g.packed.line[i] * (BAR_H + 3)" :width="Math.max(9, ax.x(callEnd(c)) - ax.x(c.at) + 6)" :height="BAR_H + 4" class="hit" />
            </g>
          </g>
          <!-- What changed the lane -->
          <g v-if="row('lane')" class="lane">
            <g v-for="(m, i) in lane" :key="`l-${i}`" :class="`tone-${m.tone}`" @mouseenter="show($event, m.title, [m.text, new Date(m.at).toLocaleTimeString()].filter(Boolean))" @mouseleave="tip = null">
              <path :d="`M${ax.x(m.at)} ${row('lane')!.y + 5} l6 6 l-6 6 l-6 -6 z`" />
              <rect :x="ax.x(m.at) - 7" :y="row('lane')!.y" width="14" :height="row('lane')!.h" class="hit" />
            </g>
          </g>
          <!-- Hooks devstack gave -->
          <g v-if="row('hooks')" class="hooks">
            <g v-for="(h, i) in t.hooks" :key="`h-${i}`" @mouseenter="show($event, `Hook: ${h.event}`, [h.hooks.length ? `text given: ${[...new Set(h.hooks.map((x) => x.skill))].join(', ')}` : 'nothing to give', new Date(h.at).toLocaleTimeString()])" @mouseleave="tip = null">
              <circle :cx="ax.x(h.at)" :cy="row('hooks')!.y + 11" r="4.5" />
              <rect :x="ax.x(h.at) - 7" :y="row('hooks')!.y" width="14" :height="row('hooks')!.h" class="hit" />
            </g>
          </g>
          <!-- Context and tokens -->
          <g v-if="ctxPath" class="ctx" data-testid="session-timeline-context">
            <path :d="ctxPath.area" class="area" />
            <path :d="ctxPath.line" class="line" />
          </g>
          <g v-if="sumPaths.length" class="sums" data-testid="session-timeline-tokens">
            <path v-for="p in sumPaths" :key="p.key" :d="p.d" :class="p.cls" />
          </g>
          <!-- Crosshair -->
          <line v-if="cross !== null" class="cross" :x1="cross" :x2="cross" :y1="AXIS_H" :y2="layout.height" />
        </svg>
        <div v-if="tip" class="tip" :class="tip.tone ? `tip-${tip.tone}` : ''" :style="{ left: `${Math.min(tip.x + 12, W - 250)}px`, top: `${tip.y + 14}px` }" role="tooltip" data-testid="session-timeline-tip">
          <strong>{{ tip.title }}</strong>
          <span v-for="(l, i) in tip.lines" :key="i">{{ l }}</span>
        </div>
      </div>
    </div>

    <div v-if="t.turns.length" class="legend small" data-testid="session-timeline-legend">
      <span class="lg-static"><i class="sw ctx-sw" /> Context: what the model read in per turn</span>
      <span class="faint">Tokens summed up:</span>
      <button v-for="l in [...LAYERS].reverse()" :key="l.key" type="button" class="lg" :class="{ off: hidden.has(l.key) }" :aria-pressed="!hidden.has(l.key)" :data-testid="`session-timeline-layer-${l.key}`" :title="hidden.has(l.key) ? 'Show in the chart' : 'Leave out of the chart'" @click="toggle(l.key)">
        <i class="sw" :class="l.cls" /> {{ l.label }}
        <b v-if="crossInfo" class="tabular">{{ tokens(crossInfo.s[l.key]) }}</b>
      </button>
      <span v-if="crossInfo" class="faint tabular">at {{ new Date(crossInfo.at).toLocaleTimeString() }} · context {{ tokens(crossInfo.c) }}</span>
    </div>
  </div>
</template>

<style scoped>
.tl {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.sum-line {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 4px 16px;
}
.sum-line .k {
  color: var(--text-muted);
  margin-right: 2px;
}
.err {
  color: var(--danger-text);
}
.grow {
  flex: 1;
}
.frame {
  display: flex;
  border: 1px solid var(--border);
  border-radius: 8px;
  overflow: hidden;
}
.labels {
  position: relative;
  flex: none;
  border-right: 1px solid var(--border);
  background: var(--surface);
}
.label {
  position: absolute;
  left: 0;
  right: 0;
  display: flex;
  flex-direction: column;
  justify-content: center;
  padding: 0 8px;
  color: var(--text-muted);
  line-height: 1.1;
}
.plot {
  position: relative;
  flex: 1;
  min-width: 0;
  overflow-x: auto;
  overflow-y: hidden;
}
svg {
  display: block;
  font-size: 10.5px;
}
.row-a {
  fill: transparent;
}
.row-b {
  fill: var(--surface);
  opacity: 0.5;
}
.ticks line {
  stroke: var(--border);
  stroke-width: 1;
}
.ticks text,
.gap text {
  fill: var(--text-faint);
}
.gap-hatch {
  stroke: var(--border-strong);
  stroke-width: 2;
}
.hit {
  fill: transparent;
}
.mark line {
  stroke: var(--text-faint);
  stroke-dasharray: 3 3;
}
.mark text {
  fill: var(--text-muted);
}
.m-interrupt line,
.m-error line {
  stroke: var(--danger);
}
.m-interrupt text,
.m-error text {
  fill: var(--danger-text);
}
.prompt path {
  fill: var(--accent);
}
.prompt text {
  fill: var(--text-2, var(--text));
  font-weight: 600;
}
.turns rect:not(.hit) {
  fill: var(--text-muted);
}
.turns rect.side {
  fill: var(--text-faint);
}
.calls .bar {
  fill: var(--c);
}
.k-devstack {
  --c: var(--accent);
}
.k-upstream {
  --c: var(--info);
}
.k-mcp {
  --c: var(--info-text);
}
.k-builtin {
  --c: var(--neutral-text);
}
.k-agent {
  --c: var(--accent-soft);
}
.k-agent .bar {
  stroke: var(--accent);
  stroke-width: 1;
}
.calls .failed .bar {
  fill: var(--danger);
}
.calls .open .bar {
  opacity: 0.6;
}
.calls g:hover .bar {
  stroke: var(--text);
  stroke-width: 1;
}
.lane path {
  fill: var(--tone, var(--text-muted));
}
.tone-ok {
  --tone: var(--ok);
}
.tone-warn {
  --tone: var(--warn);
}
.tone-danger {
  --tone: var(--danger);
}
.tone-accent {
  --tone: var(--accent);
}
.tone-info {
  --tone: var(--info);
}
.hooks circle {
  fill: var(--surface);
  stroke: var(--ok);
  stroke-width: 2;
}
.ctx .area {
  fill: var(--accent-soft);
}
.ctx .line {
  fill: none;
  stroke: var(--accent);
  stroke-width: 1.5;
}
.l-read {
  fill: var(--neutral-soft);
}
.l-write {
  fill: var(--info-soft);
}
.l-input {
  fill: var(--info);
}
.l-output {
  fill: var(--accent);
}
.sums path {
  stroke: var(--bg);
  stroke-width: 1;
}
.cross {
  stroke: var(--text-muted);
  stroke-width: 1;
  pointer-events: none;
}
.tip {
  position: absolute;
  z-index: 20;
  width: 240px;
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 8px 10px;
  border: 1px solid var(--border-strong);
  border-radius: 8px;
  background: var(--bg-elev);
  box-shadow: var(--shadow-lg);
  font-size: 12px;
  pointer-events: none;
}
.tip span {
  color: var(--text-muted);
}
.tip-danger strong {
  color: var(--danger-text);
}
.legend {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 14px;
  align-items: center;
  color: var(--text-muted);
}
.lg {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  border: 0;
  padding: 2px 4px;
  border-radius: 4px;
  background: none;
  color: inherit;
  font: inherit;
  cursor: pointer;
}
.lg:hover {
  background: var(--surface-hover);
}
.lg.off {
  opacity: 0.5;
  text-decoration: line-through;
}
.lg-static {
  display: inline-flex;
  align-items: center;
  gap: 5px;
}
.lg b {
  color: var(--text);
  font-weight: 600;
}
.sw {
  width: 10px;
  height: 10px;
  border-radius: 2px;
  display: inline-block;
}
.sw.l-read {
  background: var(--neutral-soft);
}
.sw.l-write {
  background: var(--info-soft);
}
.sw.l-input {
  background: var(--info);
}
.sw.l-output {
  background: var(--accent);
}
.ctx-sw {
  background: var(--accent-soft);
  border: 1.5px solid var(--accent);
}
</style>
