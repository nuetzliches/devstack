<script setup lang="ts">
// A worktree's lane of a stack whose daemon does not run (ADR-0078): what it
// is, and where to start it. A start runs `devstack up` in the worktree
// through the hub: the stack's daemon comes up, the main working tree's
// services stay stopped, and the lane starts here or on the pool; the full
// start page (machines, placement) takes over once the daemon runs.
import { computed } from 'vue';
import type { LaneView } from '@/api/types';
import { usePool } from '@/stores/pool';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const props = defineProps<{ lane: LaneView }>();
const stack = useStack();
const pool = usePool();
const busy = computed(() => stack.busy['lane-start'] === props.lane.name);
</script>

<template>
  <section class="card offline-lane" data-testid="offline-lane">
    <header class="card-header">
      <Icon name="gitBranch" :size="16" />
      <h2>{{ lane.branch ?? lane.name }}</h2>
      <span class="pill sm outline">not started</span>
    </header>
    <div class="card-body">
      <p class="muted">
        {{ stack.entry?.name }} does not run. Starting this worktree's lane brings its daemon up; the main working tree's services stay stopped.
      </p>
      <p class="faint small mono">{{ lane.path }}</p>
      <p v-if="lane.main_stack_file" class="faint small">It has no stack file of its own: it runs with the main working tree's.</p>
      <div class="actions">
        <button type="button" class="btn btn-primary" :disabled="busy" data-testid="offline-lane-here" @click="stack.startOfflineLane(lane, null)">
          <span v-if="busy" class="spinner" /><Icon v-else name="play" :size="14" /> Start here
        </button>
        <button
          v-if="pool.joinedAny"
          type="button"
          class="btn"
          :disabled="busy || !!lane.not_in_pool"
          :title="lane.not_in_pool ? `It cannot start in the pool: ${lane.not_in_pool}` : 'The coordinator picks a machine'"
          data-testid="offline-lane-pool"
          @click="stack.startOfflineLane(lane, 'pool')"
        >
          <Icon name="server" :size="14" /> Start in the pool
        </button>
      </div>
      <p v-if="lane.not_in_pool && pool.joinedAny" class="small faint">
        <Icon name="info" :size="13" /> It cannot start in the pool: {{ lane.not_in_pool }}
      </p>
      <p v-if="busy" class="small muted">Starting: the stack's daemon comes up first, then its start page shows each step.</p>
    </div>
  </section>
</template>

<style scoped>
.offline-lane {
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
