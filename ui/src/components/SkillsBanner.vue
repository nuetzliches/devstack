<script setup lang="ts">
// The agent skills in the stack's repository no longer match this devstack
// or its stack file: refreshed here. Whether the stack file itself is up to
// date is UpgradeBanner's: a refresh does not answer that.
import { computed, watch } from 'vue';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();

// A new devstack (the hub restarts with it) or an edited stack file changes
// what `devstack skills install` would write.
watch(
  () => [hub.version, stack.view?.config_changed] as const,
  () => void stack.loadSkills(),
);

/** `ahead`: written by a newer devstack; `older`: by an older one; `file`: the stack file changed. */
const state = computed(() => {
  const s = stack.skills;
  if (!s || !s.outdated.length) return null;
  if (s.ahead) return 'ahead';
  return s.from === s.current ? 'file' : 'older';
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
    <button v-if="state !== 'ahead'" type="button" class="btn btn-sm" :disabled="!!stack.busy.skills" title="devstack skills install: the generated skills only, a skill the project wrote itself stays" data-testid="skills-refresh" @click="stack.refreshSkills()">
      <span v-if="stack.busy.skills" class="spinner" /><Icon v-else name="restart" :size="14" /> Refresh skills
    </button>
    <button v-else-if="hub.newer" type="button" class="btn btn-sm" :disabled="hub.updating" @click="hub.installUpdate()">Update to {{ hub.newer }}</button>
  </div>
</template>
