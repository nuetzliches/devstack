<script setup lang="ts">
// A tree not started: what it is, and where to start it (ADR-0025): on this
// machine, on a node of the pool the developer picks, or in the pool with
// the coordinator choosing (ADR-0031 point 4). A start on a node shows its
// steps as the daemon reports them, so it survives navigating away and back.
// The same page for every tree (ADR-0078): a lane of a running stack (its
// daemon starts it), a lane of a stack that does not run (the hub starts it,
// the daemon comes up), and the main working tree of a stack that does not
// run (`lane` null: it runs on this machine only, on the main ports).
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { LaneView, Placement } from '@/api/types';
import type { LaneTarget } from '@/stores/pool';
import { bytes, duration, useNow } from '@/lib/format';
import { laneLabel } from '@/lib/lanes';
import { throttle } from '@/lib/throttle';
import { usePool } from '@/stores/pool';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import AgentPrompt from './AgentPrompt.vue';
import Icon from './icons/Icon.vue';
import LocalLoad from './LocalLoad.vue';
import NodeLoad from './NodeLoad.vue';

const props = defineProps<{ lane: LaneView | null }>();
const hub = useHub();
const stack = useStack();
const pool = usePool();
onBeforeUnmount(pool.watch());

/** The nodes that can take a lane now, of every pool (ADR-0034). */
const nodes = computed(() => pool.targets);
/** The main working tree (of a stack that does not run), not a lane. */
const isMain = computed(() => !props.lane);
const title = computed(() => (props.lane ? laneLabel(props.lane) : (stack.view?.branch ?? 'main')));
const path = computed(() => props.lane?.path ?? stack.entry?.repo ?? '');
const busy = computed(() => (props.lane ? stack.busy['lane-start'] === props.lane.name : !!(stack.id && hub.starting[stack.id])));
const m = computed(() => props.lane?.moving ?? null);
const running = computed(() => !!m.value && !m.value.error && m.value.kind === 'start');
/** Why no node of the pool can run it: its stack file names no lane image
 * (ADR-0028). Said before a start, not after a lease and a sync. */
const MAIN_HERE = "the main working tree runs on this machine, on the stack's main ports; a worktree's lane runs in the pool";
const notInPool = computed(() => (isMain.value ? MAIN_HERE : (props.lane?.not_in_pool ?? null)));
const failed = computed(() => (m.value?.error && m.value.kind === 'start' ? m.value : null));
const placeName = (to: string) => (to === 'pool' ? 'the pool' : pool.pools.some((p) => p.name === to) ? `the pool ${to}` : to);
const now = useNow();
/** The current phase: the step's text without the node's name. */
const current = computed(() => {
  const w = m.value?.what ?? '';
  if (w.startsWith('sync:')) return w;
  const i = w.startsWith('on ') ? w.indexOf(': ') : -1;
  return i >= 0 ? w.slice(i + 2) : w;
});
/** Lanes running on a node now, of its pool's leases. */
const lanesOn = (poolName: string, node: string) => (pool.overviews[poolName]?.leases ?? []).filter((l) => l.node === node).length;
/** The stack file's profiles (ADR-0041): how the lane runs for a purpose. */
const profiles = computed(() => stack.view?.profiles ?? []);
const profile = ref('');
const chosen = computed(() => profiles.value.find((p) => p.name === profile.value) ?? null);
/** Where each pool's coordinator would start this lane, best first, with why
 * (ADR-0026 point 3): its own order, not one guessed here; by what the
 * lane's runs with the chosen profile took (ADR-0049). */
const placement = ref<Record<string, Placement[]>>({});
async function loadPlacement() {
  const repo = stack.view?.name;
  if (!repo || !props.lane) return;
  const out: Record<string, Placement[]> = {};
  for (const p of pool.placeable) {
    try {
      const r = await api().request<Placement[]>('GET', paths.poolPlacement(p.pool, repo, props.lane.name, profile.value || undefined));
      if (r.ok && Array.isArray(r.body)) out[p.pool] = r.body;
    } catch {
      // An older coordinator has no order to give: the cards show the numbers.
    }
  }
  placement.value = out;
}
watch(() => [props.lane?.name, stack.view?.name, pool.placeable.map((p) => p.pool).join(), profile.value], () => void loadPlacement(), { immediate: true });
// Read again when a pool's nodes or leases changed (ADR-0073), at most every 10 s.
const placementAgain = throttle(() => void loadPlacement(), 10_000);
watch(() => pool.placeable.map((p) => `${pool.feeds[p.pool]?.versions.nodes}|${pool.feeds[p.pool]?.versions.leases}`).join(), placementAgain);
onBeforeUnmount(placementAgain.cancel);
/** What the lane would take where it starts (ADR-0049 point 4), and where
 * that comes from: its footprint in the pool, else the estimate. */
const need = computed(() => {
  const p = Object.values(placement.value)
    .flat()
    .find((x) => x.need);
  return p?.need ? { cpus: p.need.cpus, memory: p.need.memory, from: p.need_from ?? '', steady: p.steady } : null;
});
/** A node's place in its pool's order: its rank (1: the pick) and why. */
function placeOf(poolName: string, nodeId: string) {
  const list = placement.value[poolName] ?? [];
  const i = list.findIndex((p) => p.node === nodeId);
  return i < 0 ? null : { rank: i + 1, pick: i === 0 && list[0].eligible, ...list[i] };
}
/** The node the lane is starting on, while it does: the one named, or the
 * one its lease is on. Its card says so rather than rank the node for a
 * lane that holds room there already. */
function startingOn(n: LaneTarget) {
  const mv = m.value;
  if (!running.value || !mv) return false;
  if (mv.to === n.on || mv.to === n.node.name) return true;
  const lease = `lease on ${n.node.name}`;
  return [mv.what, ...(mv.phases ?? []).map((p) => p.phase)].some((w) => w === lease || w.startsWith(`${lease}:`) || w.startsWith(`on ${n.node.name}:`));
}

/** The node a pool's coordinator would pick, if it said. */
const pickOf = (poolName: string) => {
  const first = placement.value[poolName]?.[0];
  // Only a node of that pool's own (an order for another pool's nodes shows nothing).
  return first?.eligible && nodes.value.some((n) => n.pool === poolName && n.node.id === first.node) ? first : null;
};
/** The cards by pool, each pool's in its coordinator's order (its pick first). */
const groups = computed(() =>
  pool.pools
    .map((p) => ({
      pool: p.name,
      nodes: nodes.value
        .filter((n) => n.pool === p.name)
        .sort((a, b) => (placeOf(a.pool, a.node.id)?.rank ?? 1e9) - (placeOf(b.pool, b.node.id)?.rank ?? 1e9)),
    }))
    .filter((g) => g.nodes.length),
);
/** Where the start button under the pointer (or focus) would put the lane: its cards stand out, the others step back. */
const hl = ref<{ local: true } | { pool: string } | null>(null);
/** Whether the lit cards are a pool's pick (its colour) or all its nodes (no pick known). */
const litPick = computed(() => !!hl.value && 'pool' in hl.value && !!pickOf(hl.value.pool));
function lit(n: LaneTarget | null) {
  const h = hl.value;
  if (!h) return null;
  if ('local' in h) return !n;
  if (!n || n.pool !== h.pool) return false;
  const pick = pickOf(h.pool);
  return pick ? pick.node === n.node.id : true;
}

function start(on?: string) {
  if (!props.lane) void stack.startStack();
  else if (!stack.running) void stack.startOfflineLane(props.lane, on ?? null, profile.value || undefined);
  else void stack.startLane(props.lane.name, on, profile.value || undefined);
}
</script>

<template>
  <!-- Nothing of this tree runs: said first, across the page, unlike any running tree's view. -->
  <div class="not-running" data-testid="tree-not-running">
    <Icon name="power" :size="26" class="nr-icon" />
    <div class="nr-text">
      <strong>{{ isMain ? `${stack.entry?.name ?? 'The stack'} is not running` : `${title} is not running` }}</strong>
      <span v-if="isMain">Nothing of it runs on this machine. Its main working tree starts here, on the stack's main ports.</span>
      <span v-else-if="!stack.running">{{ stack.entry?.name }} does not run either: its daemon comes up with this lane, its main working tree stays stopped.</span>
      <span v-else>{{ stack.entry?.name }} runs; this worktree's lane does not yet. Start it below, here or on a machine of the pool.</span>
    </div>
    <span class="pill sm outline">not started</span>
  </div>
  <section class="card lane-start" data-testid="lane-start">
    <header class="card-header">
      <Icon name="gitBranch" :size="18" />
      <h2>{{ title }}</h2>
      <span class="faint small">{{ isMain ? 'main working tree' : 'worktree' }}</span>
      <span class="pill sm outline">not started</span>
    </header>
    <div class="card-body">
      <p v-if="isMain" class="muted">
        {{ stack.entry?.name }} does not run. Its main working tree runs on this machine, on the stack's main ports; on another machine of the pool,
        a worktree's lane runs.
      </p>
      <p v-else-if="!stack.running" class="muted">
        {{ stack.entry?.name }} does not run. Starting this lane brings the stack's daemon up, the main working tree's services stay stopped; it runs
        here, or on another machine of the pool: everything of the lane runs there, its ports are forwarded here.
      </p>
      <p v-else class="muted">
        This worktree's lane runs nothing yet. Start it here, or on another machine of the pool: everything of the lane runs there (build,
        services, its own infrastructure), its ports are forwarded here.
      </p>
      <p class="mono small faint">{{ path }}</p>
      <p v-if="lane?.main_stack_file" class="faint small">It has no stack file of its own: it runs with the main working tree's.</p>

      <div v-if="running && m" class="progress" data-testid="lane-start-progress">
        <span class="bar"><span :style="{ width: `${(100 * m.step) / m.steps}%` }" /></span>
        <span>
          Starting on <strong>{{ placeName(m.to) }}</strong> · step {{ m.step }} of {{ m.steps }} · {{ duration(now - m.since) }}
        </span>
      </div>
      <p v-else-if="failed" class="err" data-testid="lane-start-failed">
        <Icon name="alert" :size="14" /> Starting on {{ placeName(failed.to) }} failed: {{ failed.error }}
      </p>
      <!-- Its phases: the lane has no pages until its daemon runs on the node. -->
      <ol v-if="m && (m.phases?.length || running)" class="phases" data-testid="lane-start-phases">
        <li v-for="(p, i) in m.phases ?? []" :key="i" class="done">
          <Icon name="checkCircle" :size="13" />
          <span class="ph">{{ p.phase }}</span>
          <span class="t tabular">{{ duration(p.ms) }}</span>
        </li>
        <li v-if="running" class="now">
          <span class="spinner sm" aria-hidden="true" />
          <span class="ph">{{ current }}</span>
          <span class="t tabular">{{ duration(now - (m.phase_since ?? m.since)) }}</span>
        </li>
        <li v-else-if="failed" class="failed">
          <Icon name="xCircle" :size="13" />
          <span class="ph">{{ current }}</span>
        </li>
      </ol>
      <p v-if="running" class="faint small">The lane's pages open once its daemon runs on the node; its containers and services show their own start there.</p>

      <label v-if="profiles.length && !isMain" class="profile" data-testid="lane-start-profile">
        <span>Profile</span>
        <select v-model="profile" class="input">
          <option value="">none: as the stack file says</option>
          <option v-for="p in profiles" :key="p.name" :value="p.name">{{ p.name }}{{ p.description ? ` · ${p.description}` : '' }}</option>
        </select>
        <span v-if="chosen?.fresh" class="faint small">starts with fresh data: its databases are seeded again</span>
      </label>
      <div class="choices">
        <button
          type="button"
          class="btn btn-primary"
          :disabled="busy || running"
          data-testid="lane-start-here"
          @click="start()"
          @mouseenter="hl = { local: true }"
          @mouseleave="hl = null"
          @focus="hl = { local: true }"
          @blur="hl = null"
        >
          <span v-if="busy && !running" class="spinner" /><Icon v-else name="play" :size="14" /> Start here
        </button>
        <button
          v-if="!pool.placeable.length"
          type="button"
          class="btn"
          disabled
          title="No node of the pool can take a lane now"
          data-testid="lane-start-pool"
        >
          <Icon name="server" :size="14" /> Start in the pool
        </button>
        <button
          v-for="p in pool.placeable"
          :key="p.pool"
          type="button"
          class="btn"
          :class="{ 'pick-btn': pickOf(p.pool) }"
          :disabled="busy || running || !!notInPool"
          :title="
            notInPool
              ? `No node can run it: ${notInPool}`
              : placement[p.pool]?.[0]?.eligible
                ? `The coordinator would start it on ${placement[p.pool][0].name}: ${placement[p.pool][0].why}`
                : 'The coordinator picks the node: warm first, then free capacity'
          "
          data-testid="lane-start-pool"
          @click="start(p.on)"
          @mouseenter="hl = { pool: p.pool }"
          @mouseleave="hl = null"
          @focus="hl = { pool: p.pool }"
          @blur="hl = null"
        >
          <Icon name="server" :size="14" /> Start in the pool{{ p.pool === 'default' ? '' : ` ${p.pool}` }}
          <span v-if="pickOf(p.pool)" class="to">→ {{ pickOf(p.pool)!.name }}</span>
        </button>
      </div>
      <p v-if="isMain && pool.joinedAny" class="small faint" data-testid="lane-start-main-here">
        <Icon name="info" :size="13" /> It does not start in the pool: {{ MAIN_HERE }}. Pick or make one in the branch menu above, or in the sidebar.
      </p>
      <div v-else-if="notInPool && pool.joinedAny" class="no-image small" data-testid="lane-start-no-image">
        <Icon name="info" :size="14" />
        <span class="grow">
          <strong>It cannot start in the pool:</strong> the stack file it runs with names no lane image, the Linux image a node runs the lane in
          (<code>[lanes] image</code> or <code>[lanes] dockerfile</code>). A coding agent picks one with the stack's toolchains.
        </span>
        <AgentPrompt v-if="stack.id && hub.reachable && !stack.readOnly" compact label="Name one" kind="lane-image" :target="{ stack: stack.id, path: path }" />
      </div>
      <p v-if="need && nodes.length && !notInPool" class="need small" data-testid="lane-start-need">
        <span class="swatch" aria-hidden="true" />
        <span class="said"
          >The lane starts with up to <strong>{{ need.cpus.toFixed(1) }} CPU</strong> and <strong>{{ bytes(need.memory) }}</strong>, its build
          included<template v-if="need.steady"
            >, then runs on about <strong>{{ need.steady.cpus.toFixed(1) }} CPU</strong> and
            <strong>{{ bytes(need.steady.memory) }}</strong></template
          >
          ({{ need.from }}).</span
        >
        <span class="faint">Each chart, on the scale of all the machine has: above, the last hour (dashed: what a node offers the pool); below, now, with the lane on top.</span>
      </p>
      <!-- A card opens the machine's page (its name is the link, over the whole
           card); its button starts the lane there. -->
      <div v-if="nodes.length" class="targets" data-testid="lane-start-targets">
        <p class="group"><Icon name="monitor" :size="13" /> Here</p>
        <div class="target local" :class="{ lit: lit(null) === true, dim: lit(null) === false }">
          <span class="tname">
            <Icon name="monitor" :size="14" />
            <RouterLink :to="{ name: 'pool-here' }" class="open" title="This machine: what it has, runs and reported">this machine</RouterLink>
            <button type="button" class="btn btn-xs btn-primary go" :disabled="busy || running" title="Start the lane on this machine" data-testid="lane-start-local" @click="start()">
              <Icon name="play" :size="12" /> Start
            </button>
          </span>
          <LocalLoad :need="need ?? undefined" />
        </div>
        <template v-for="g in groups" :key="g.pool">
        <p class="group" data-testid="lane-start-group">
          <Icon name="server" :size="13" /> {{ g.pool === 'default' ? 'The pool' : `The pool ${g.pool}` }}
          <span class="faint">· {{ g.nodes.length }} machine{{ g.nodes.length === 1 ? '' : 's' }}<template v-if="pickOf(g.pool)">, its coordinator would pick {{ pickOf(g.pool)!.name }}</template></span>
        </p>
        <div v-for="n in g.nodes" :key="n.on" class="target" :class="{ lit: lit(n) === true, pick: lit(n) === true && litPick, dim: lit(n) === false }" data-testid="lane-start-target">
          <span class="tname">
            <Icon name="server" :size="14" /> on
            <RouterLink :to="{ name: 'pool-machine', params: { pool: n.pool, id: n.node.id } }" class="open" :title="`${n.label}: its page in the pool`" data-testid="lane-start-open">{{ n.node.name }}</RouterLink>
            <span v-if="placeOf(n.pool, n.node.id)?.pick" class="pill sm info" data-testid="lane-start-pick" :title="`The coordinator would start it here: ${placeOf(n.pool, n.node.id)?.why}`">pool's pick</span>
            <button
              type="button"
              class="btn btn-xs go"
              :class="{ 'pick-btn': placeOf(n.pool, n.node.id)?.pick }"
              :disabled="busy || running || !!notInPool"
              :title="placeOf(n.pool, n.node.id)?.eligible === false ? `It cannot take it: ${placeOf(n.pool, n.node.id)?.why}` : `Start the lane on ${n.label}`"
              data-testid="lane-start-node"
              @click="start(n.on)"
            >
              <Icon name="play" :size="12" /> Start
            </button>
          </span>
          <span v-if="startingOn(n)" class="why" data-testid="lane-start-why">the lane is starting here</span>
          <span v-else-if="placeOf(n.pool, n.node.id)" class="why" :class="{ err: !placeOf(n.pool, n.node.id)?.eligible }" data-testid="lane-start-why">
            {{ placeOf(n.pool, n.node.id)?.eligible ? `#${placeOf(n.pool, n.node.id)?.rank} · ` : 'cannot take it: ' }}{{ placeOf(n.pool, n.node.id)?.why
            }}<template v-if="placeOf(n.pool, n.node.id)?.eligible && placeOf(n.pool, n.node.id)?.disk"> · needs {{ bytes(placeOf(n.pool, n.node.id)?.disk) }} of disk there</template>
          </span>
          <NodeLoad :pool="n.pool" :node="n.node" :lanes="lanesOn(n.pool, n.node.id)" :need="need ?? undefined" />
        </div>
        </template>
      </div>
      <p v-if="!pool.joinedAny" class="muted small">
        Lanes on other machines need a pool: <RouterLink :to="{ name: 'pool' }">join one</RouterLink>.
      </p>
    </div>
  </section>
</template>

<style scoped>
.lane-start {
  max-width: 1800px;
  margin: 16px 0;
}
.not-running {
  display: flex;
  align-items: center;
  gap: 16px;
  margin-top: 16px;
  padding: 22px 26px;
  border: 1.5px dashed var(--border-strong);
  border-radius: 12px;
  color: var(--text-muted);
  background: repeating-linear-gradient(135deg, transparent 0 10px, color-mix(in srgb, var(--text-faint) 6%, transparent) 10px 20px);
}
.nr-icon {
  flex: none;
  color: var(--text-faint);
}
.nr-text {
  display: flex;
  flex-direction: column;
  gap: 4px;
  flex: 1;
  min-width: 0;
}
.nr-text strong {
  color: var(--text);
  font-size: 15px;
}
.choices {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 12px;
}
.no-image {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 10px;
}
.no-image .grow {
  flex: 1;
}
.progress {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 10px;
  font-size: 13px;
}
.bar {
  width: 140px;
  height: 6px;
  border-radius: 3px;
  background: var(--surface-hover);
  overflow: hidden;
  flex: none;
}
.bar > span {
  display: block;
  height: 100%;
  background: var(--accent);
  transition: width 0.3s;
}
.err {
  color: var(--danger);
  display: flex;
  gap: 6px;
  align-items: center;
}
.small {
  font-size: 12px;
}
.mono {
  font-family: var(--font-mono, monospace);
}
.profile {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  margin: 8px 0;
  font-size: 13px;
}
.profile select {
  max-width: 360px;
}
.phases {
  list-style: none;
  margin: 10px 0 4px;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
  font-size: 12.5px;
}
.phases li {
  display: flex;
  align-items: center;
  gap: 8px;
}
.phases .done {
  color: var(--text-muted);
}
.phases .done :deep(svg) {
  color: var(--ok);
}
.phases .now .ph {
  font-weight: 600;
}
.phases .failed {
  color: var(--danger);
}
.phases .t {
  margin-left: auto;
  color: var(--text-muted);
}
.phases .ph {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.targets {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 8px;
  margin-top: 10px;
}
.target {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface);
}
.target:hover {
  border-color: var(--accent);
  background: var(--surface-hover);
}
/* The name is the card's link: its box covers the card. The button sits above it. */
.open {
  color: inherit;
  font-weight: 700;
  text-decoration: none;
}
.open::after {
  content: '';
  position: absolute;
  inset: 0;
  border-radius: inherit;
}
.open:focus-visible {
  outline: none;
}
.target:has(.open:focus-visible) {
  box-shadow: var(--focus);
}
.group {
  grid-column: 1 / -1;
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 8px 0 0;
  font-size: 12.5px;
  font-weight: 600;
}
.target {
  transition: opacity 0.15s, border-color 0.15s;
}
.target.dim {
  opacity: 0.4;
}
/* Lit by the start button under the pointer: blue here, the pick's colour for a pool's pick, plain for a pool's nodes when it named none. */
.target.lit {
  border-color: var(--text-muted);
  box-shadow: 0 0 0 1px var(--text-muted);
}
.target.lit.pick {
  border-color: var(--info);
  box-shadow: 0 0 0 1px var(--info);
}
.target.local.lit {
  border-color: var(--accent);
  box-shadow: 0 0 0 1px var(--accent);
}
/* The pool's pick, in its colour: the pill, its card's button and the pool's start. */
.pick-btn {
  border-color: color-mix(in srgb, var(--info) 55%, var(--border));
  background: var(--info-soft);
  color: var(--info-text);
}
.pick-btn:hover:not(:disabled) {
  border-color: var(--info);
  background: color-mix(in srgb, var(--info) 22%, var(--surface));
}
.pick-btn .to {
  color: inherit;
  opacity: 0.8;
}
.to {
  color: var(--text-muted);
  font-weight: 500;
}
.go {
  position: relative;
  z-index: 1;
  margin-left: auto;
}
.tname {
  display: flex;
  align-items: center;
  gap: 6px;
}
.need {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 4px;
  margin: 12px 0 0;
}
.need .faint {
  flex-basis: 100%;
}
/* One flex item: the gap would part its words from their commas. */
.need .said {
  flex: 1;
  min-width: 0;
}
.swatch {
  display: inline-block;
  width: 10px;
  height: 10px;
  border-radius: 2px;
  background: repeating-linear-gradient(-45deg, var(--accent) 0 2px, color-mix(in srgb, var(--accent) 30%, transparent) 2px 4px);
}
.why {
  font-size: 12px;
  color: var(--text-muted);
}
.why.err {
  color: var(--danger);
}
</style>
