<script setup lang="ts">
// The stack's daemon should restart: it runs another devstack than the one
// installed, or a stack file changed since it started. Above the stack's page
// and above every lane's views, since a lane is where a stack file gets edited
// (ADR-0081); the page says it of the main tree's file, a lane's view of the
// shown lane's own.
import { computed } from 'vue';
import { restartLanesText } from '@/lib/lanes';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const props = defineProps<{ configChanged: boolean }>();

const hub = useHub();
const stack = useStack();

const hubDown = computed(() => hub.status === 'unreachable' || hub.stream === 'error');
const name = computed(() => stack.entry?.name ?? stack.mainView?.name ?? null);
/** The daemon runs another devstack than the hub: a restart of the stack starts it with the installed one. */
const daemonOutdated = computed(() => {
  const d = stack.entry?.daemon?.version;
  const h = hub.version;
  return d && h && d !== h ? { daemon: d, hub: h } : null;
});
const reason = computed(() => {
  if (!stack.running || hubDown.value) return null;
  if (daemonOutdated.value) return 'version';
  if (props.configChanged) return 'config';
  return null;
});
const lanesText = computed(() => restartLanesText(stack.restartLanes));
const canRestart = computed(() => !stack.readOnly && !!stack.entry);
</script>

<template>
  <div v-if="reason" class="banner warn" :data-testid="reason === 'version' ? 'daemon-outdated' : 'config-changed'">
    <Icon :name="reason === 'version' ? 'alert' : 'file'" :size="18" />
    <span v-if="reason === 'version'" class="grow">
      <strong>{{ name ?? 'This stack' }} runs devstack {{ daemonOutdated?.daemon }}, devstack {{ daemonOutdated?.hub }} is installed.</strong>
      {{ ' ' }}A restart of the stack switches it over<template v-if="configChanged"> and applies the changed stack file</template>.
      <template v-if="lanesText">{{ ' ' }}{{ lanesText }}</template>
    </span>
    <span v-else class="grow"><strong>The stack file changed since {{ name ?? 'the stack' }} started.</strong> It applies after a restart.</span>
    <button v-if="canRestart" type="button" class="btn btn-primary btn-sm" :disabled="!!stack.busy.stack" data-testid="banner-restart-stack" @click="stack.restartStack()">
      <span v-if="stack.busy.stack === 'restart'" class="spinner" /><Icon v-else name="restart" :size="14" />
      {{ stack.busy.stack === 'restart' ? 'Reloading…' : 'Reload' }}
    </button>
  </div>
</template>

<style scoped>
.banner {
  flex-wrap: wrap;
}
.grow {
  flex: 1 1 240px;
}
</style>
