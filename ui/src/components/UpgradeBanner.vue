<script setup lang="ts">
// The stack file was last brought up to date with an older devstack
// (`[stack] reviewed`): a coding agent brings it up to date with the hub's
// prompt. The agent skills are SkillsBanner's: refreshing them leaves this.
import { watch } from 'vue';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import AgentPrompt from './AgentPrompt.vue';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();

// A new devstack (the hub restarts with it) or an edited stack file (an
// agent set `reviewed`) changes the answer.
watch(
  () => [hub.version, stack.view?.config_changed] as const,
  () => void stack.loadUpgrade(),
);
</script>

<template>
  <div v-if="stack.upgrade?.behind" class="banner info" data-testid="upgrade-behind">
    <Icon name="info" :size="18" />
    <span class="grow">
      <strong>
        <template v-if="stack.upgrade.reviewed">The stack file of {{ stack.entry?.name ?? 'this repository' }} was last brought up to date with devstack {{ stack.upgrade.reviewed }}, devstack {{ stack.upgrade.current }} is installed.</template>
        <template v-else>{{ stack.entry?.name ?? 'This repository' }} was set up with {{ stack.upgrade.from ? `devstack ${stack.upgrade.from}` : 'an earlier devstack' }}, devstack {{ stack.upgrade.current }} is installed.</template>
      </strong>
      A coding agent can bring it up to date: what changed in between, the stack file, the agent skills. It notes the version in <code>[stack] reviewed</code>.
      <AgentPrompt v-if="stack.id" kind="upgrade" :target="{ stack: stack.id }" />
    </span>
  </div>
</template>
