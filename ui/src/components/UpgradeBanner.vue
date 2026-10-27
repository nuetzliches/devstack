<script setup lang="ts">
// The stack file was last brought up to date with an older devstack
// (`[stack] reviewed`): a coding agent brings it up to date with the hub's
// prompt, the agent skills included: the dashboard writes no file of a
// repository itself (ADR-0071). One line; skills from an older devstack are
// said here, not in SkillsBanner, and hidden with it until the next minor
// devstack is installed.
import { computed, watch } from 'vue';
import { skillsState, useUpgradeNotice } from '@/lib/upgradeNotice';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import AgentPrompt from './AgentPrompt.vue';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();
const notice = useUpgradeNotice();

const name = computed(() => stack.entry?.name ?? 'This repository');
const skillsOlder = computed(() => skillsState(stack.skills) === 'older');

// A new devstack (the hub restarts with it) or an edited stack file (an
// agent set `reviewed`) changes the answer.
watch(
  () => [hub.version, stack.view?.config_changed] as const,
  () => void stack.loadUpgrade(),
);
</script>

<template>
  <div v-if="notice.shown.value && stack.upgrade" class="banner neutral upgrade" data-testid="upgrade-behind">
    <Icon name="info" :size="18" />
    <span class="grow" title="A coding agent brings it up to date: what changed in between, the stack file, the agent skills. It notes the version in [stack] reviewed.">
      <strong>
        <template v-if="stack.upgrade.reviewed">{{ name }} was last brought up to date with devstack {{ stack.upgrade.reviewed }}</template>
        <template v-else>{{ name }} was set up with {{ stack.upgrade.from ? `devstack ${stack.upgrade.from}` : 'an earlier devstack' }}</template></strong>, {{ stack.upgrade.current }} is installed<template v-if="skillsOlder">; the agent skills come from {{ stack.skills!.from ? `devstack ${stack.skills!.from}` : 'an earlier one' }}</template>.
    </span>
    <AgentPrompt v-if="stack.id" compact label="Bring up to date" kind="upgrade" :target="{ stack: stack.id }" />
    <button type="button" class="btn btn-ghost btn-sm btn-icon" title="Hide until the next minor devstack" aria-label="Hide" data-testid="upgrade-behind-dismiss" @click="notice.hide">
      <Icon name="x" :size="14" />
    </button>
  </div>
</template>

<style scoped>
.upgrade {
  flex-wrap: wrap;
  padding-top: 6px;
  padding-bottom: 6px;
}
.upgrade .grow {
  min-width: 240px;
}
</style>
