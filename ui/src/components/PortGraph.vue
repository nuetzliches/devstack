<script setup lang="ts">
// The port world of a stack (ADR-0082 point 2), on its page: the machine's
// fixed ports of the stack, the lane each leads to (the one serving the main
// ports marked), and each lane's holders with their own ports; a lane on a
// node by the ports forwarded here. A port another stack uses too is red and
// names it. Each lane is a band of a grid, the connectors an SVG layer
// measured from the elements; on a narrow card a grouped list says the same.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import type { PortEntry, PortOverlap } from '@/api/types';
import { portGraph, useText, type GraphHolder, type GraphLane, type GraphPort } from '@/lib/portGraph';
import Icon from '@/components/icons/Icon.vue';

const props = defineProps<{
  /** The stack's id. */
  stack: string;
  entries: PortEntry[] | null;
  overlaps: PortOverlap[];
  /** The lanes known, `main` first. */
  lanes: { name: string; node?: string | null }[];
  servedBy?: string | null;
  /** Its daemon runs: else the entries are what its stack file declares. */
  running: boolean;
  /** Neither the daemon nor the hub answered. */
  error?: string | null;
}>();
const emit = defineEmits<{ open: [lane: string] }>();

/** Instance ports: noise for most readers, shown on request. */
const instance = ref(false);
const graph = computed(() =>
  portGraph({ stack: props.stack, entries: props.entries ?? [], overlaps: props.overlaps, lanes: props.lanes, servedBy: props.servedBy, instance: instance.value }),
);
/** The ports this stack shares with another, each once. */
const shared = computed(() => {
  const out = new Map<string, GraphPort>();
  for (const b of graph.value.lanes) {
    for (const p of [...b.inbound, ...b.holders.flatMap((h) => h.ports)]) if (p.overlap && !out.has(p.label)) out.set(p.label, p);
  }
  return [...out.values()].sort((a, b) => a.port - b.port);
});

// ------------------------------------------------------------ highlight

/** The node hovered or focused: its path is highlighted, the rest dimmed. */
const active = ref<string | null>(null);
const lit = computed(() => new Set(active.value ? (graph.value.related[active.value] ?? [active.value]) : []));
function look(id: string, extra: Record<string, boolean> = {}) {
  return { 'pg-on': lit.value.has(id), 'pg-dim': !!active.value && !lit.value.has(id), ...extra };
}
function portClass(p: GraphPort) {
  return look(p.id, { 'pg-overlap': !!p.overlap, 'pg-muted': !p.listening, 'pg-instance': p.kind === 'instance' });
}
function holderClass(h: GraphHolder) {
  return look(h.id, { 'pg-overlap': h.overlap, 'pg-muted': !h.listening });
}
function laneClass(b: GraphLane) {
  return look(b.id, { 'pg-serves': b.servesMain });
}
function holderTitle(h: GraphHolder): string {
  const what = h.container ? 'container' : 'service';
  return `${what} ${h.name} in ${h.lane === 'main' ? 'main' : `lane ${h.lane}`}${h.listening ? '' : ', not listening'}${h.overlap ? '; a port of it is used by another stack too' : ''}`;
}
function laneTitle(b: GraphLane): string {
  return `${b.name === 'main' ? 'main (the main working tree)' : `lane ${b.name}`}${b.node ? `, on node ${b.node}` : ', on this machine'}${b.servesMain ? ', serves the main ports' : ''}`;
}

// ------------------------------------------------------------ connectors

const box = ref<HTMLElement | null>(null);
type Wire = { id: string; d: string; muted: boolean; overlap: boolean; on: boolean; label?: string };
const geometry = ref<{ id: string; d: string }[]>([]);
const size = ref({ w: 0, h: 0 });

function measure() {
  const el = box.value;
  if (!el) return;
  const at = el.getBoundingClientRect();
  size.value = { w: at.width, h: at.height };
  if (!at.width) {
    geometry.value = [];
    return;
  }
  const nodes = new Map<string, DOMRect>();
  for (const n of el.querySelectorAll<HTMLElement>('[data-node]')) nodes.set(n.dataset.node!, n.getBoundingClientRect());
  const out: { id: string; d: string }[] = [];
  for (const e of graph.value.edges) {
    const a = nodes.get(e.from);
    const b = nodes.get(e.to);
    if (!a || !b || !a.width || !b.width) continue;
    const x1 = a.right - at.left;
    const y1 = a.top + a.height / 2 - at.top;
    const x2 = b.left - at.left;
    const y2 = b.top + b.height / 2 - at.top;
    const dx = Math.max(12, (x2 - x1) / 2);
    out.push({ id: e.id, d: `M${x1.toFixed(1)} ${y1.toFixed(1)} C${(x1 + dx).toFixed(1)} ${y1.toFixed(1)} ${(x2 - dx).toFixed(1)} ${y2.toFixed(1)} ${x2.toFixed(1)} ${y2.toFixed(1)}` });
  }
  geometry.value = out;
}
const wires = computed<Wire[]>(() => {
  const by = new Map(graph.value.edges.map((e) => [e.id, e]));
  return geometry.value
    .map((g) => {
      const e = by.get(g.id);
      return e ? { ...g, muted: e.muted, overlap: e.overlap, on: lit.value.has(e.from) && lit.value.has(e.to), ...(e.label ? { label: e.label } : {}) } : null;
    })
    .filter((w): w is Wire => w !== null);
});

let observer: ResizeObserver | null = null;
let frame = 0;
function remeasure() {
  cancelAnimationFrame(frame);
  frame = requestAnimationFrame(measure);
}
onMounted(() => {
  if (typeof ResizeObserver !== 'undefined') {
    observer = new ResizeObserver(remeasure);
    watch(
      box,
      (el, old) => {
        if (old) observer?.unobserve(old);
        if (el) observer?.observe(el);
      },
      { immediate: true },
    );
  }
  void nextTick(measure);
});
onBeforeUnmount(() => {
  observer?.disconnect();
  cancelAnimationFrame(frame);
});
watch(graph, () => void nextTick(measure));
</script>

<template>
  <section class="card" data-testid="stack-ports">
    <header class="card-header">
      <Icon name="link" :size="16" />
      <h2>Ports</h2>
      <span
        v-if="graph.overlaps"
        class="pill sm danger"
        :title="`${graph.overlaps} port${graph.overlaps === 1 ? '' : 's'} another stack on this machine uses too`"
        data-testid="stack-ports-overlaps"
      >
        {{ graph.overlaps }} overlap{{ graph.overlaps === 1 ? '' : 's' }}
      </span>
      <span class="grow" />
      <button
        type="button"
        class="chip-toggle"
        :aria-pressed="instance"
        title="The ports of the running instances behind the proxy or a dev server's front: handed out by the OS, new with every start"
        data-testid="stack-ports-instance"
        @click="instance = !instance"
      >
        instance ports<span v-if="!instance && graph.hiddenInstance" class="faint">&nbsp;{{ graph.hiddenInstance }}</span>
      </button>
    </header>

    <div v-if="error && !entries" class="card-body muted small" data-testid="stack-ports-error">{{ error }}</div>
    <div v-else-if="!entries" class="card-body muted small">Loading…</div>
    <div v-else-if="!graph.lanes.length" class="card-body muted small">No port known.</div>
    <div v-else class="pg">
      <p v-if="!running" class="pg-note muted small">What its stack file declares: nothing of it runs.</p>

      <div ref="box" class="pg-graph" data-testid="stack-ports-graph" @mouseleave="active = null">
        <svg class="pg-wires" :viewBox="`0 0 ${size.w} ${size.h}`" :width="size.w" :height="size.h" aria-hidden="true">
          <path v-for="w in wires" :key="w.id" :d="w.d" :class="{ 'pg-muted': w.muted, 'pg-overlap': w.overlap, 'pg-on': w.on, 'pg-dim': !!active && !w.on }">
            <title v-if="w.label">forwarded from {{ w.label }}</title>
          </path>
        </svg>
        <div class="pg-head">Fixed ports</div>
        <div class="pg-head">Lanes</div>
        <div class="pg-head">Holders</div>

        <template v-for="b in graph.lanes" :key="b.id">
          <div class="pg-cell pg-in" :data-lane="b.name">
            <span v-if="b.node && b.inbound.some((p) => p.kind === 'node')" class="pg-caption faint small">forwarded from {{ b.node }}</span>
            <span
              v-for="p in b.inbound"
              :key="p.id"
              class="pg-port mono"
              :class="portClass(p)"
              :data-node="p.id"
              :title="p.title"
              :aria-label="p.title"
              tabindex="0"
              data-testid="stack-port"
              :data-port="p.label"
              @mouseenter="active = p.id"
              @focus="active = p.id"
              @blur="active = null"
            >{{ p.label }}</span>
          </div>
          <div class="pg-cell pg-mid">
            <span
              class="pg-lane"
              :class="laneClass(b)"
              :data-node="b.id"
              :title="laneTitle(b)"
              :aria-label="laneTitle(b)"
              role="link"
              tabindex="0"
              data-testid="stack-ports-lane"
              :data-lane="b.name"
              @mouseenter="active = b.id"
              @focus="active = b.id"
              @blur="active = null"
              @click="emit('open', b.name)"
              @keydown.enter="emit('open', b.name)"
            >
              <strong class="truncate">{{ b.name }}</strong>
              <span v-if="b.servesMain" class="pill sm ok">main ports</span>
              <span v-if="b.node" class="pill sm info truncate">{{ b.node }}</span>
            </span>
          </div>
          <div class="pg-cell pg-out">
            <div
              v-for="h in b.holders"
              :key="h.id"
              class="pg-holder"
              :class="holderClass(h)"
              :data-node="h.id"
              :title="holderTitle(h)"
              :aria-label="holderTitle(h)"
              tabindex="0"
              data-testid="stack-ports-holder"
              @mouseenter="active = h.id"
              @focus="active = h.id"
              @blur="active = null"
            >
              <span class="pg-name truncate" :class="{ mono: h.container }">{{ h.name }}</span>
              <span
                v-for="p in h.ports"
                :key="p.id"
                class="pg-port pg-chip mono"
                :class="portClass(p)"
                :data-node="p.id"
                :title="p.title"
                :aria-label="p.title"
                tabindex="0"
                data-testid="stack-port"
                :data-port="p.label"
                @mouseenter.stop="active = p.id"
                @mouseleave="active = h.id"
                @focus.stop="active = p.id"
                @blur="active = null"
              >{{ p.label }}</span>
            </div>
          </div>
        </template>
      </div>

      <!-- The same, grouped, where three columns do not fit. -->
      <ul class="pg-list" data-testid="stack-ports-list">
        <li v-for="b in graph.lanes" :key="b.id" class="pg-list-lane">
          <div class="pg-list-head">
            <a href="#" class="pg-list-name" @click.prevent="emit('open', b.name)">{{ b.name }}</a>
            <span v-if="b.servesMain" class="pill sm ok">main ports</span>
            <span v-if="b.node" class="pill sm info">{{ b.node }}</span>
          </div>
          <ul>
            <li v-for="h in b.holders" :key="h.id" :class="{ 'pg-muted-text': !h.listening }">
              <span :class="{ mono: h.container }">{{ h.name }}</span>
              <span
                v-for="p in [...b.inbound.filter((x) => x.holder === h.name), ...h.ports]"
                :key="p.id"
                class="pg-port mono"
                :class="{ 'pg-overlap': !!p.overlap, 'pg-muted': !p.listening, 'pg-instance': p.kind === 'instance' }"
                :title="p.title"
                :aria-label="p.title"
                >{{ p.label }}<template v-if="p.kind === 'node'"> ← {{ p.node }}</template></span
              >
            </li>
            <li v-for="p in b.inbound.filter((x) => !x.holder)" :key="p.id">
              <span class="faint">unknown</span>
              <span class="pg-port mono" :class="{ 'pg-overlap': !!p.overlap, 'pg-muted': !p.listening }" :title="p.title" :aria-label="p.title">{{ p.label }}</span>
            </li>
          </ul>
        </li>
      </ul>

      <p v-if="graph.idle.length" class="pg-idle small muted" data-testid="stack-ports-idle">
        Not started, no ports: <span class="mono">{{ graph.idle.join(', ') }}</span>
      </p>
      <ul v-if="shared.length" class="pg-shared small" data-testid="stack-ports-shared">
        <li v-for="p in shared" :key="p.id">
          <span class="pg-port pg-overlap mono">{{ p.label }}</span>
          also used by {{ p.overlap!.map(useText).join('; ') }}
        </li>
      </ul>
    </div>
  </section>
</template>

<style scoped>
.small {
  font-size: 12px;
}
.grow {
  flex: 1;
  min-width: 0;
}
.pg {
  container-type: inline-size;
  padding: var(--space-3) var(--space-4) var(--space-4);
}
.pg-note {
  margin: 0 0 var(--space-3);
}

/* ------------------------------------------------------------ the graph */
.pg-graph {
  position: relative;
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 0.9fr) minmax(0, 1.5fr);
  column-gap: clamp(32px, 7%, 80px);
  row-gap: var(--space-3);
  align-items: center;
}
.pg-wires {
  position: absolute;
  inset: 0;
  overflow: visible;
  pointer-events: none;
}
.pg-wires path {
  fill: none;
  stroke: var(--border-strong);
  stroke-width: 1.5;
  transition: stroke 0.12s var(--ease), opacity 0.12s var(--ease);
}
.pg-wires path.pg-muted {
  stroke-dasharray: 4 4;
}
.pg-wires path.pg-overlap {
  stroke: var(--danger);
}
.pg-wires path.pg-on {
  stroke: var(--accent);
  stroke-width: 2.25;
}
.pg-wires path.pg-overlap.pg-on {
  stroke: var(--danger);
}
.pg-wires path.pg-dim {
  opacity: 0.35;
}
.pg-head {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-muted);
}
.pg-cell {
  position: relative;
  min-width: 0;
}
.pg-in {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 6px;
}
.pg-caption {
  flex-basis: 100%;
  text-align: right;
}
.pg-mid {
  display: flex;
}
.pg-out {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 6px;
}
.pg-port {
  display: inline-flex;
  align-items: center;
  height: 22px;
  padding: 0 7px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border-strong);
  background: var(--surface-2);
  color: var(--text-2);
  font-size: 11.5px;
  white-space: nowrap;
  cursor: default;
}
.pg-lane {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 100%;
  padding: 6px 10px;
  border-radius: var(--radius);
  border: 1px solid var(--border-strong);
  background: var(--surface);
  cursor: pointer;
}
.pg-lane.pg-serves {
  border-color: color-mix(in srgb, var(--ok) 55%, transparent);
  background: var(--ok-soft);
}
.pg-holder {
  display: inline-flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  max-width: 100%;
  padding: 4px 8px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--surface-2);
}
.pg-name {
  font-weight: 550;
  font-size: 12.5px;
}
.pg-chip {
  height: 20px;
  background: var(--surface);
}
.pg-instance {
  border-style: dotted;
}
.pg-muted {
  border-style: dashed;
  color: var(--text-faint);
}
.pg-muted .pg-name {
  color: var(--text-faint);
}
.pg-overlap {
  border-color: var(--danger);
  background: var(--danger-soft);
  color: var(--danger-text);
}
.pg-on {
  border-color: var(--accent);
  box-shadow: 0 0 0 1px var(--accent);
}
.pg-overlap.pg-on {
  border-color: var(--danger);
  box-shadow: 0 0 0 1px var(--danger);
}
.pg-dim {
  opacity: 0.5;
}
.pg-port:focus-visible,
.pg-lane:focus-visible,
.pg-holder:focus-visible {
  outline: none;
  box-shadow: var(--focus);
}

/* ------------------------------------------------------------ the list */
.pg-list {
  display: none;
  list-style: none;
  margin: 0;
  padding: 0;
}
.pg-list ul {
  list-style: none;
  margin: 4px 0 0;
  padding: 0 0 0 var(--space-4);
}
.pg-list-lane + .pg-list-lane {
  margin-top: var(--space-3);
  padding-top: var(--space-3);
  border-top: 1px solid var(--border);
}
.pg-list-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}
.pg-list-name {
  font-weight: 600;
  color: inherit;
}
.pg-list li li {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  padding: 3px 0;
  font-size: 12.5px;
}
.pg-muted-text {
  color: var(--text-faint);
}

@container (max-width: 560px) {
  .pg-graph {
    display: none;
  }
  .pg-list {
    display: block;
  }
}

/* ------------------------------------------------------------ below */
.pg-idle {
  margin: var(--space-3) 0 0;
}
.pg-shared {
  list-style: none;
  margin: var(--space-3) 0 0;
  padding: var(--space-2) var(--space-3);
  border-radius: var(--radius-sm);
  background: var(--danger-soft);
  color: var(--danger-text);
}
.pg-shared li {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  padding: 2px 0;
}
</style>
