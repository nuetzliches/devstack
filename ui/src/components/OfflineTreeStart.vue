<script setup lang="ts">
// A tree of a stack whose daemon does not run (ADR-0078), shown the same for
// the main working tree and for a worktree's lane: what it is, and where to
// start it. The main tree runs on this machine only, on the main ports
// (ADR-0025); a worktree's lane starts here or on the pool, through the hub
// (`devstack up` in it: the daemon comes up, the main tree's services stay
// stopped). Once the daemon runs, the tree's own page takes over.
import { computed } from 'vue';
import type { LaneView } from '@/api/types';
import { useHub } from '@/stores/hub';
import { usePool } from '@/stores/pool';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

/** `lane`: a worktree's lane; none: the main working tree. */
const props = defineProps<{ lane?: LaneView | null }>();
const stack = useStack();
const hub = useHub();
const pool = usePool();

const main = computed(() => !props.lane);
const title = computed(() => (props.lane ? (props.lane.branch ?? props.lane.name) : (stack.view?.branch ?? 'main')));
const path = computed(() => (props.lane ? props.lane.path : (stack.entry?.repo ?? '')));
const busy = computed(() => (props.lane ? stack.busy['lane-start'] === props.lane.name : !!(stack.id && hub.starting[stack.id])));
/** Why it does not start in the pool; `null`: it can. */
const notInPool = computed(() => (main.value ? "the main working tree runs on this machine; a worktree's lane runs in the pool" : (props.lane?.not_in_pool ?? null)));

function startHere() {
  if (props.lane) void stack.startOfflineLane(props.lane, null);
  else void stack.startStack();
}
function startInPool() {
  if (props.lane) void stack.startOfflineLane(props.lane, 'pool');
}
</script>

<template>
  <section class="card offline-tree" data-testid="offline-tree" :data-kind="main ? 'main' : 'lane'">
    <header class="card-header">
      <Icon name="gitBranch" :size="16" />
      <h2>{{ title }}</h2>
      <span class="faint small">{{ main ? 'main working tree' : 'worktree' }}</span>
      <span class="pill sm outline">not started</span>
    </header>
    <div class="card-body">
      <p class="muted">
        {{ stack.entry?.name }} does not run.
        <template v-if="main">Its main working tree runs on this machine, on the stack's main ports.</template>
        <template v-else>Starting this lane brings the stack's daemon up; the main working tree's services stay stopped.</template>
      </p>
      <p class="faint small mono">{{ path }}</p>
      <p v-if="lane?.main_stack_file" class="faint small">It has no stack file of its own: it runs with the main working tree's.</p>
      <div class="actions">
        <button type="button" class="btn btn-primary" :disabled="busy" data-testid="offline-tree-here" @click="startHere">
          <span v-if="busy" class="spinner" /><Icon v-else name="play" :size="14" /> {{ busy ? 'Starting…' : 'Start here' }}
        </button>
        <button
          v-if="pool.joinedAny"
          type="button"
          class="btn"
          :disabled="busy || !!notInPool"
          :title="notInPool ? `It does not start in the pool: ${notInPool}` : 'The coordinator picks a machine'"
          data-testid="offline-tree-pool"
          @click="startInPool"
        >
          <Icon name="server" :size="14" /> Start in the pool
        </button>
      </div>
      <p v-if="notInPool && pool.joinedAny" class="small faint">
        <Icon name="info" :size="13" /> It does not start in the pool: {{ notInPool }}.
      </p>
    </div>
  </section>
</template>

<style scoped>
.offline-tree {
  max-width: 760px;
}
.actions {
  display: flex;
  gap: 8px;
  margin: 12px 0 6px;
}
.mono {
  font-family: var(--font-mono, monospace);
}
</style>
