<script setup lang="ts">
// The agent skills in the stack's repository no longer match this devstack
// or its stack file: refreshed here, or, after an update of devstack, the
// whole setup brought up to date by a coding agent with the hub's prompt.
import { computed, watch } from 'vue';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import AgentPrompt from './AgentPrompt.vue';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();

// A new devstack (the hub restarts with it) or an edited stack file changes
// what `devstack skills install` would write.
watch(
  () => [hub.version, stack.view?.config_changed] as const,
  () => void stack.loadSkills(),
);

/** `ahead`: written by a newer devstack; `upgrade`: by an older one; `file`: the stack file changed. */
const state = computed(() => {
  const s = stack.skills;
  if (!s || !s.outdated.length) return null;
  if (s.ahead) return 'ahead';
  return s.from === s.current ? 'file' : 'upgrade';
});
</script>

<template>
  <div v-if="state" class="banner" :class="state === 'upgrade' ? 'info' : 'neutral'" data-testid="skills-outdated" :data-state="state">
    <Icon :name="state === 'upgrade' ? 'info' : 'file'" :size="18" />
    <span class="grow">
      <template v-if="state === 'upgrade'">
        <strong>{{ stack.entry?.name ?? 'This repository' }} was set up with {{ stack.skills!.from ? `devstack ${stack.skills!.from}` : 'an earlier devstack' }}, devstack {{ stack.skills!.current }} is installed.</strong>
        A coding agent can bring it up to date: what changed in between, the stack file, the agent skills.
        <AgentPrompt v-if="stack.id" kind="upgrade" :target="{ stack: stack.id }" />
      </template>
      <template v-else-if="state === 'file'">
        <strong>The agent skills no longer match the stack file.</strong> Refreshed, they name its services and commands again.
      </template>
      <template v-else>
        <strong>The agent skills come from devstack {{ stack.skills!.from }}, this is {{ stack.skills!.current }}.</strong> Update devstack rather than write older ones.
      </template>
    </span>
    <button v-if="state !== 'ahead'" type="button" class="btn btn-sm" :disabled="!!stack.busy.skills" title="devstack skills install: the generated skills only, a skill the project wrote itself stays" data-testid="skills-refresh" @click="stack.refreshSkills()">
      <span v-if="stack.busy.skills" class="spinner" /><Icon v-else name="restart" :size="14" /> Refresh skills
    </button>
    <button v-else-if="hub.newer" type="button" class="btn btn-sm" :disabled="hub.updating" @click="hub.installUpdate()">Update to {{ hub.newer }}</button>
  </div>
</template>
