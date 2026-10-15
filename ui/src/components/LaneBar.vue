<script setup lang="ts">
// Lanes (plan §3.5): other git worktrees running the stack beside the main
// one. Pick which one the views show, hand the main ports to a lane and back,
// remove a lane. Shown only when the stack has lanes.
import { computed } from 'vue';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const stack = useStack();
const current = computed(() => stack.lanes.find((l) => l.name === stack.lane) ?? null);
const servedBy = computed(() => stack.lanes.find((l) => l.owns_main)?.name ?? null);
const busy = computed(() => !!stack.busy['lane-main']);

function pick(e: Event) {
  const v = (e.target as HTMLSelectElement).value;
  stack.selectLane(v === '' ? null : v);
}
</script>

<template>
  <div v-if="stack.lanes.length" class="lanebar" data-testid="lane-bar">
    <label class="pick">
      <Icon name="gitBranch" :size="14" />
      <span class="sr-only">Lane</span>
      <select :value="stack.lane ?? ''" class="select" data-testid="lane-select" @change="pick">
        <option value="">main working tree</option>
        <option v-for="l in stack.lanes" :key="l.name" :value="l.name">{{ l.name }} (+{{ l.port_offset }}){{ l.owns_main ? ' · main ports' : '' }}</option>
      </select>
    </label>
    <button
      v-if="current && !current.owns_main"
      type="button"
      class="btn btn-sm"
      :disabled="busy"
      title="This lane's services answer on the main ports; workers move with them. Dev servers stay with the main tree."
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
      :title="`Stop lane ${current.name} and forget it (the worktree stays)`"
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
