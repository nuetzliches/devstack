<script setup lang="ts">
// The main working tree of a stack that does not run, as a stopped lane is
// shown (ADR-0078): where it can start. It runs on this machine only, with
// the main ports; on the pool a worktree's lane runs instead (ADR-0025).
import { computed } from 'vue';
import { useHub } from '@/stores/hub';
import { usePool } from '@/stores/pool';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const stack = useStack();
const hub = useHub();
const pool = usePool();
const starting = computed(() => !!(stack.id && hub.starting[stack.id]));
const branch = computed(() => stack.view?.branch ?? null);
</script>

<template>
  <section class="card offline-main" data-testid="offline-main">
    <header class="card-header">
      <Icon name="gitBranch" :size="16" />
      <h2>{{ branch ?? 'main' }}</h2>
      <span class="faint small">main working tree</span>
      <span class="pill sm outline">not started</span>
    </header>
    <div class="card-body">
      <p class="muted">{{ stack.entry?.name }} does not run. Its main working tree runs on this machine, on the stack's main ports.</p>
      <p class="faint small mono">{{ stack.entry?.repo }}</p>
      <div class="actions">
        <button type="button" class="btn btn-primary" :disabled="starting" data-testid="offline-main-start" @click="stack.startStack()">
          <span v-if="starting" class="spinner" /><Icon v-else name="play" :size="14" /> {{ starting ? 'Starting…' : 'Start here' }}
        </button>
      </div>
      <p v-if="pool.joinedAny" class="small faint">
        <Icon name="info" :size="13" /> It does not run in the pool: a worktree's lane does. Pick or make one in the branch menu above
        <template v-if="stack.offlineLanes.length"> or in the sidebar</template>.
      </p>
    </div>
  </section>
</template>

<style scoped>
.offline-main {
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
