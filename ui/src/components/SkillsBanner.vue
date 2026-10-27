<script setup lang="ts">
// The agent skills in the stack's repository no longer match this devstack
// or its stack file. A coding agent refreshes them, from the hub's prompt:
// the dashboard writes no file of a repository (ADR-0071). While the stack
// file is behind, skills from an older devstack are UpgradeBanner's, whose
// prompt refreshes them too, shown or hidden with it: one notice, not two.
import { computed, watch } from 'vue';
import { skillsState } from '@/lib/upgradeNotice';
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

const state = computed(() => {
  const s = skillsState(stack.skills);
  return s === 'older' && stack.upgrade?.behind ? null : s;
});
</script>

<template>
  <div v-if="state" class="banner neutral" data-testid="skills-outdated" :data-state="state">
    <Icon name="file" :size="18" />
    <span class="grow">
      <template v-if="state === 'file'">
        <strong>The agent skills no longer match the stack file.</strong> Refreshed, they name its services and commands again.
      </template>
      <template v-else-if="state === 'older'">
        <strong>The agent skills come from {{ stack.skills!.from ? `devstack ${stack.skills!.from}` : 'an earlier devstack' }}, this is {{ stack.skills!.current }}.</strong> Refreshed, they describe this one.
      </template>
      <template v-else>
        <strong>The agent skills come from devstack {{ stack.skills!.from }}, this is {{ stack.skills!.current }}.</strong> Update devstack rather than write older ones.
      </template>
    </span>
    <AgentPrompt v-if="state !== 'ahead' && stack.id" compact label="Refresh skills" kind="skills" :target="{ stack: stack.id }" />
    <button v-else-if="hub.newer" type="button" class="btn btn-sm" :disabled="hub.updating" @click="hub.installUpdate()">Update to {{ hub.newer }}</button>
  </div>
</template>
