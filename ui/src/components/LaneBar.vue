<script setup lang="ts">
// Lanes (plan §3.5): the repository's other git worktrees, running the stack
// beside the main one. Pick which one the views show (picking one not started
// starts it), hand the main ports to a lane and back, remove a lane with its
// worktree. Shown only when the repository has other worktrees.
import { computed } from 'vue';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const stack = useStack();
const current = computed(() => stack.lanes.find((l) => l.name === stack.lane) ?? null);
const servedBy = computed(() => stack.lanes.find((l) => l.owns_main)?.name ?? null);
const busy = computed(() => !!stack.busy['lane-main']);

const starting = computed(() => stack.busy['lane-start'] ?? null);

function pick(e: Event) {
  const select = e.target as HTMLSelectElement;
  const v = select.value;
  const lane = stack.lanes.find((l) => l.name === v);
  if (lane && lane.active === false) {
    select.value = stack.lane ?? '';
    void stack.startLane(v);
    return;
  }
  stack.selectLane(v === '' ? null : v);
}

function label(l: (typeof stack.lanes)[number]): string {
  const branch = l.branch && l.branch !== l.name ? ` · ${l.branch}` : '';
  const state = l.active === false ? ' · not started' : ` (+${l.port_offset})`;
  return `${l.name}${branch}${state}${l.owns_main ? ' · main ports' : ''}`;
}
</script>

<template>
  <div v-if="stack.lanes.length" class="lanebar" data-testid="lane-bar">
    <label class="pick">
      <Icon name="gitBranch" :size="14" />
      <span class="sr-only">Lane</span>
      <select :value="stack.lane ?? ''" class="select" :disabled="!!starting" data-testid="lane-select" @change="pick">
        <option value="">main working tree</option>
        <option v-for="l in stack.lanes" :key="l.name" :value="l.name" :title="l.path">{{ label(l) }}</option>
      </select>
    </label>
    <span v-if="starting" class="muted" style="font-size: 12px" data-testid="lane-starting"><span class="spinner" /> starting {{ starting }}…</span>
    <button
      v-if="current && !current.owns_main"
      type="button"
      class="btn btn-sm"
      :disabled="busy"
      title="This lane's services, dev servers included, answer on the main ports; workers move with them."
      data-testid="lane-take-main"
      @click="stack.laneMain(current.name)"
    >
      <span v-if="busy" class="spinner" /><Icon v-else name="promote" :size="14" /> Serve main ports
    </button>
    <button v-if="servedBy" type="button" class="btn btn-sm" :disabled="busy" data-testid="lane-main-back" @click="stack.laneMain('main')">
      <Icon name="rollback" :size="14" /> Main ports back to the main tree
    </button>
    <button
      v-if="current && !current.owns_main"
      type="button"
      class="btn btn-ghost btn-sm btn-icon"
      :title="`Stop lane ${current.name} and remove its worktree`"
      :aria-label="`Remove lane ${current.name}`"
      data-testid="lane-remove"
      @click="stack.removeLane(current.name)"
    >
      <Icon name="trash" :size="14" />
    </button>
  </div>
</template>

<style scoped>
.lanebar {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-left: auto;
  flex-wrap: wrap;
}
.pick {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
}
.select {
  font: inherit;
  font-size: 13px;
  padding: 3px 6px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface);
  color: inherit;
  max-width: 260px;
}
</style>
