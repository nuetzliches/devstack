<script setup lang="ts">
// A worktree whose lane is not started: what it is, and where to start it
// (ADR-0025): on this machine, on a node of the pool the developer picks, or
// in the pool with the coordinator choosing (ADR-0031 point 4). A start on a
// node shows its steps as the daemon reports them, so it survives
// navigating away and back.
import { computed, onBeforeUnmount } from 'vue';
import type { LaneView } from '@/api/types';
import { laneLabel } from '@/lib/lanes';
import { usePool } from '@/stores/pool';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const props = defineProps<{ lane: LaneView }>();
const stack = useStack();
const pool = usePool();
onBeforeUnmount(pool.watch(30_000));

/** The nodes that can take a lane now, of every pool (ADR-0034). */
const nodes = computed(() => pool.targets);
const busy = computed(() => stack.busy['lane-start'] === props.lane.name);
const m = computed(() => props.lane.moving ?? null);
const running = computed(() => !!m.value && !m.value.error && m.value.kind === 'start');
const failed = computed(() => (m.value?.error && m.value.kind === 'start' ? m.value : null));
const placeName = (to: string) => (to === 'pool' ? 'the pool' : pool.pools.some((p) => p.name === to) ? `the pool ${to}` : to);

function start(on?: string) {
  void stack.startLane(props.lane.name, on);
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
          Starting on <strong>{{ placeName(m.to) }}</strong> · step {{ m.step }} of {{ m.steps }}: {{ m.what }}
        </span>
      </div>
      <p v-else-if="failed" class="err" data-testid="lane-start-failed">
        <Icon name="alert" :size="14" /> Starting on {{ placeName(failed.to) }} failed: {{ failed.error }}
      </p>

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
          :disabled="busy || running"
          title="The coordinator picks the node with the most free memory"
          data-testid="lane-start-pool"
          @click="start(p.on)"
        >
          <Icon name="server" :size="14" /> Start in the pool{{ p.pool === 'default' ? '' : ` ${p.pool}` }}
        </button>
        <button
          v-for="n in nodes"
          :key="n.on"
          type="button"
          class="btn"
          :disabled="busy || running"
          :title="`${n.node.report!.cpus.toFixed(1)} CPU free`"
          data-testid="lane-start-node"
          @click="start(n.on)"
        >
          on {{ n.label }}
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
</style>
