<script setup lang="ts">
// The stack's devstack.toml is missing (a setup broken off after the stack
// was registered, or the file removed since) or does not load: nothing of
// the main tree starts, and there is no version to hold against this
// devstack, so UpgradeBanner has nothing to say. A coding agent sets it up
// or repairs it from the hub's prompt (ADR-0071). Not dismissable: it goes
// once the file loads.
import { watch } from 'vue';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import AgentPrompt from './AgentPrompt.vue';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();

// The agent wrote, removed or edited the file.
watch(
  () => [hub.version, stack.entry?.no_file, stack.view?.config_changed] as const,
  () => void stack.loadUpgrade(),
);
</script>

<template>
  <div v-if="stack.entry && stack.fileProblem === 'missing'" class="banner warn" data-testid="stack-file-missing">
    <Icon name="file" :size="18" />
    <span class="grow">
      <strong>{{ stack.entry.name }} is not set up yet:</strong> <code>{{ stack.entry.file }}</code> does not exist, so nothing of it starts.
      A coding agent sets it up: it runs <code>devstack init</code> for a first draft, works through it, validates and starts the stack.
      <span class="faint">Not wanted any more? The stack's menu in the sidebar removes it from the dashboard.</span>
    </span>
    <AgentPrompt v-if="!stack.readOnly" compact label="Set it up" kind="setup" :target="{ path: stack.entry.repo }" />
  </div>
  <div v-else-if="stack.entry && stack.fileProblem === 'invalid'" class="banner warn" data-testid="stack-file-invalid">
    <Icon name="alert" :size="18" />
    <span class="grow">
      <strong>The stack file does not load:</strong>
      <code v-if="stack.upgrade?.error" class="err">{{ stack.upgrade.error }}</code>
      <template v-if="/needs devstack/.test(stack.upgrade?.error ?? '')"> It needs a newer devstack.</template>
    </span>
    <template v-if="/needs devstack/.test(stack.upgrade?.error ?? '')">
      <button v-if="hub.newer" type="button" class="btn btn-sm" :disabled="hub.updating" @click="hub.installUpdate()">Update to {{ hub.newer }}</button>
      <span v-else class="faint">Run <code>devstack update</code>.</span>
    </template>
    <AgentPrompt v-else-if="!stack.readOnly" compact label="Repair it" kind="fix-file" :target="{ path: stack.entry.repo }" />
  </div>
</template>

<style scoped>
.banner {
  flex-wrap: wrap;
}
.grow {
  flex: 1 1 240px;
}
.err {
  white-space: pre-wrap;
}
</style>
