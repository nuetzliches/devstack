<script setup lang="ts">
// A worktree whose lane is not started: what it is, and where to start it
// (ADR-0025): on this machine, on a node of the pool the developer picks, or
// in the pool with the coordinator choosing (ADR-0031 point 4). A start on a
// node shows its steps as the daemon reports them, so it survives
// navigating away and back.
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

const props = defineProps<{ lane: LaneView }>();
const hub = useHub();
const stack = useStack();
const pool = usePool();
onBeforeUnmount(pool.watch());

/** The nodes that can take a lane now, of every pool (ADR-0034). */
const nodes = computed(() => pool.targets);
const busy = computed(() => stack.busy['lane-start'] === props.lane.name);
const m = computed(() => props.lane.moving ?? null);
const running = computed(() => !!m.value && !m.value.error && m.value.kind === 'start');
/** Why no node of the pool can run it: its stack file names no lane image
 * (ADR-0028). Said before a start, not after a lease and a sync. */
const notInPool = computed(() => props.lane.not_in_pool ?? null);
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
  if (!repo) return;
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
watch(() => [props.lane.name, stack.view?.name, pool.placeable.map((p) => p.pool).join(), profile.value], () => void loadPlacement(), { immediate: true });
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

function start(on?: string) {
  void stack.startLane(props.lane.name, on, profile.value || undefined);
}
</script>

<template>
  <section class="card lane-start" data-testid="lane-start">
    <header class="card-header">
      <Icon name="gitBranch" :size="18" />
      <h2>{{ laneLabel(lane) }}</h2>
      <span class="pill sm outline">not started</span>
    </header>
    <div class="card-body">
      <p class="muted">
        This worktree's lane runs nothing yet. Start it here, or on another machine of the pool: everything of the lane runs there (build,
        services, its own infrastructure), its ports are forwarded here.
      </p>
      <p class="mono small faint">{{ lane.path }}</p>

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

      <label v-if="profiles.length" class="profile" data-testid="lane-start-profile">
        <span>Profile</span>
        <select v-model="profile" class="input">
          <option value="">none: as the stack file says</option>
          <option v-for="p in profiles" :key="p.name" :value="p.name">{{ p.name }}{{ p.description ? ` · ${p.description}` : '' }}</option>
        </select>
        <span v-if="chosen?.fresh" class="faint small">starts with fresh data: its databases are seeded again</span>
      </label>
      <div class="choices">
        <button type="button" class="btn btn-primary" :disabled="busy || running" data-testid="lane-start-here" @click="start()">
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
        >
          <Icon name="server" :size="14" /> Start in the pool{{ p.pool === 'default' ? '' : ` ${p.pool}` }}
        </button>
      </div>
      <div v-if="notInPool && pool.joinedAny" class="no-image small" data-testid="lane-start-no-image">
        <Icon name="info" :size="14" />
        <span class="grow">
          <strong>It cannot start in the pool:</strong> the stack file it runs with names no lane image, the Linux image a node runs the lane in
          (<code>[lanes] image</code> or <code>[lanes] dockerfile</code>). A coding agent picks one with the stack's toolchains.
        </span>
        <AgentPrompt v-if="stack.id && hub.reachable && !stack.readOnly" compact label="Name one" kind="lane-image" :target="{ stack: stack.id, path: lane.path }" />
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
      <div v-if="nodes.length" class="targets" data-testid="lane-start-targets">
        <button type="button" class="target local" :disabled="busy || running" data-testid="lane-start-local" @click="start()">
          <span class="tname"><Icon name="monitor" :size="14" /> <strong>this machine</strong></span>
          <LocalLoad :need="need ?? undefined" />
        </button>
        <button
          v-for="n in nodes"
          :key="n.on"
          type="button"
          class="target"
          :disabled="busy || running || !!notInPool"
          data-testid="lane-start-node"
          @click="start(n.on)"
        >
          <span class="tname">
            <Icon name="server" :size="14" /> on <strong>{{ n.label }}</strong>
            <span v-if="placeOf(n.pool, n.node.id)?.pick" class="pill sm info" data-testid="lane-start-pick" :title="`The coordinator would start it here: ${placeOf(n.pool, n.node.id)?.why}`">pool's pick</span>
          </span>
          <span v-if="startingOn(n)" class="why" data-testid="lane-start-why">the lane is starting here</span>
          <span v-else-if="placeOf(n.pool, n.node.id)" class="why" :class="{ err: !placeOf(n.pool, n.node.id)?.eligible }" data-testid="lane-start-why">
            {{ placeOf(n.pool, n.node.id)?.eligible ? `#${placeOf(n.pool, n.node.id)?.rank} · ` : 'cannot take it: ' }}{{ placeOf(n.pool, n.node.id)?.why
            }}<template v-if="placeOf(n.pool, n.node.id)?.eligible && placeOf(n.pool, n.node.id)?.disk"> · needs {{ bytes(placeOf(n.pool, n.node.id)?.disk) }} of disk there</template>
          </span>
          <NodeLoad :pool="n.pool" :node="n.node" :lanes="lanesOn(n.pool, n.node.id)" :need="need ?? undefined" />
        </button>
      </div>
      <p v-if="!pool.joinedAny" class="muted small">
        Lanes on other machines need a pool: <RouterLink :to="{ name: 'pool' }">join one</RouterLink>.
      </p>
    </div>
  </section>
</template>

<style scoped>
.lane-start {
  max-width: 820px;
  margin: 16px 0;
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
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface);
  color: inherit;
  font: inherit;
  cursor: pointer;
  text-align: left;
}
.target:hover:not(:disabled) {
  border-color: var(--accent);
  background: var(--surface-hover);
}
.target.local {
  border-color: color-mix(in srgb, var(--accent) 55%, var(--border));
  background: color-mix(in srgb, var(--accent) 8%, var(--surface));
}
.target:disabled {
  opacity: 0.6;
  cursor: default;
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
