<script setup lang="ts">
// Lanes (plan §3.5, ADR-0024): the repository's other git worktrees, running
// the stack beside the main one. The stack switcher picks the lane shown; this
// bar says which one it is and acts on it: hand the main ports to it and back,
// stop it. Worktrees themselves are git's: devstack never creates or removes
// one. Shown only when the repository has other worktrees.
import { computed } from 'vue';
import { useStack } from '@/stores/stack';
import { laneLabel } from '@/lib/lanes';
import Icon from './icons/Icon.vue';

const stack = useStack();
const current = computed(() => stack.lanes.find((l) => l.name === stack.lane) ?? null);
const servedBy = computed(() => stack.lanes.find((l) => l.owns_main) ?? null);
const busy = computed(() => !!stack.busy['lane-main']);
const starting = computed(() => stack.busy['lane-start'] ?? null);
const startingLabel = computed(() => {
  const l = stack.lanes.find((x) => x.name === starting.value);
  return l ? laneLabel(l) : starting.value;
});
</script>

<template>
  <div v-if="stack.lanes.length" class="lanebar" data-testid="lane-bar">
    <span v-if="current" class="chip" :title="current.path" data-testid="lane-current">
      <Icon name="gitBranch" :size="14" /> {{ laneLabel(current) }} <span class="muted">+{{ current.port_offset }}</span>
    </span>
    <span v-else class="chip muted" data-testid="lane-current"><Icon name="gitBranch" :size="14" /> main working tree</span>
    <span v-if="starting" class="muted" style="font-size: 12px" data-testid="lane-starting"><span class="spinner" /> starting {{ startingLabel }}…</span>
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
      class="btn btn-ghost btn-sm"
      :title="`Stop lane ${laneLabel(current)}: its services stop; the worktree stays`"
      data-testid="lane-stop"
      @click="stack.stopLane(current.name)"
    >
      <Icon name="stop" :size="14" /> Stop lane
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
.chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  max-width: 320px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
