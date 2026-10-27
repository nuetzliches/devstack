<script setup lang="ts">
// The stack file was last brought up to date with an older devstack
// (`[stack] reviewed`): a coding agent brings it up to date with the hub's
// prompt. The agent skills are SkillsBanner's: refreshing them leaves this.
// Hidden, it stays hidden until the next minor devstack is installed.
import { computed, ref, watch } from 'vue';
import { persist } from '@/lib/persist';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import AgentPrompt from './AgentPrompt.vue';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();

/** `0.25.1` and `0.25.0-rc.1` are `0.25`: a patch release does not show it again. */
function minor(version: string): string {
  return version.split(/[.-]/).slice(0, 2).join('.');
}

const key = computed(() => `upgradeDismissed.${stack.id}`);
const dismissed = ref<string | null>(null);
watch(key, (k) => (dismissed.value = persist.get<string>(k)), { immediate: true });
const shown = computed(() => !!stack.upgrade?.behind && dismissed.value !== minor(stack.upgrade.current));

function dismiss() {
  if (!stack.upgrade) return;
  dismissed.value = minor(stack.upgrade.current);
  persist.set(key.value, dismissed.value);
}

// A new devstack (the hub restarts with it) or an edited stack file (an
// agent set `reviewed`) changes the answer.
watch(
  () => [hub.version, stack.view?.config_changed] as const,
  () => void stack.loadUpgrade(),
);
</script>

<template>
  <div v-if="shown && stack.upgrade" class="banner info" data-testid="upgrade-behind">
    <Icon name="info" :size="18" />
    <span class="grow">
      <strong>
        <template v-if="stack.upgrade.reviewed">The stack file of {{ stack.entry?.name ?? 'this repository' }} was last brought up to date with devstack {{ stack.upgrade.reviewed }}, devstack {{ stack.upgrade.current }} is installed.</template>
        <template v-else>{{ stack.entry?.name ?? 'This repository' }} was set up with {{ stack.upgrade.from ? `devstack ${stack.upgrade.from}` : 'an earlier devstack' }}, devstack {{ stack.upgrade.current }} is installed.</template>
      </strong>
      A coding agent can bring it up to date: what changed in between, the stack file, the agent skills. It notes the version in <code>[stack] reviewed</code>.
      <AgentPrompt v-if="stack.id" kind="upgrade" :target="{ stack: stack.id }" />
    </span>
    <button type="button" class="btn btn-ghost btn-sm btn-icon" title="Hide until the next minor devstack" aria-label="Hide" data-testid="upgrade-behind-dismiss" @click="dismiss">
      <Icon name="x" :size="14" />
    </button>
  </div>
</template>
