<script setup lang="ts">
// The agent skills in the stack's repository no longer match this devstack
// or its stack file: refreshed here, or, after an update of devstack, the
// whole setup brought up to date by a coding agent with a ready prompt.
import { computed, watch } from 'vue';
import { upgradePrompt } from '@/lib/prompts';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import { useUi } from '@/stores/ui';
import Icon from './icons/Icon.vue';

const hub = useHub();
const stack = useStack();
const ui = useUi();

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

const prompt = computed(() => {
  const s = stack.skills;
  const e = stack.entry;
  if (!s || !e) return '';
  return upgradePrompt({ repo: e.repo, file: e.file, from: s.from, current: s.current });
});

async function copyPrompt() {
  try {
    await navigator.clipboard.writeText(prompt.value);
    ui.toast('success', 'Prompt copied', 'Paste it into your coding agent, in any folder.');
  } catch {
    ui.toast('warning', 'Copy failed', 'Open "Show the prompt" and copy it yourself.');
  }
}
</script>

<template>
  <div v-if="state" class="banner" :class="state === 'upgrade' ? 'info' : 'neutral'" data-testid="skills-outdated" :data-state="state">
    <Icon :name="state === 'upgrade' ? 'info' : 'file'" :size="18" />
    <span class="grow">
      <template v-if="state === 'upgrade'">
        <strong>{{ stack.entry?.name ?? 'This repository' }} was set up with {{ stack.skills!.from ? `devstack ${stack.skills!.from}` : 'an earlier devstack' }}, devstack {{ stack.skills!.current }} is installed.</strong>
        A coding agent can bring it up to date: what changed in between, the stack file, the agent skills.
      </template>
      <template v-else-if="state === 'file'">
        <strong>The agent skills no longer match the stack file.</strong> Refreshed, they name its services and commands again.
      </template>
      <template v-else>
        <strong>The agent skills come from devstack {{ stack.skills!.from }}, this is {{ stack.skills!.current }}.</strong> Update devstack rather than write older ones.
      </template>
      <details v-if="state === 'upgrade'" class="prompt">
        <summary>Show the prompt</summary>
        <pre data-testid="skills-prompt">{{ prompt }}</pre>
      </details>
    </span>
    <template v-if="state !== 'ahead'">
      <button v-if="state === 'upgrade'" type="button" class="btn btn-primary btn-sm" data-testid="skills-copy-prompt" @click="copyPrompt">
        <Icon name="copy" :size="14" /> Copy prompt
      </button>
      <button type="button" class="btn btn-sm" :disabled="!!stack.busy.skills" title="devstack skills install: the generated skills only, a skill the project wrote itself stays" data-testid="skills-refresh" @click="stack.refreshSkills()">
        <span v-if="stack.busy.skills" class="spinner" /><Icon v-else name="restart" :size="14" /> Refresh skills
      </button>
    </template>
    <button v-else-if="hub.newer" type="button" class="btn btn-sm" :disabled="hub.updating" @click="hub.installUpdate()">Update to {{ hub.newer }}</button>
  </div>
</template>

<style scoped>
.prompt {
  margin-top: 6px;
}
.prompt pre {
  white-space: pre-wrap;
  font-size: 12px;
  max-height: 240px;
  overflow: auto;
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 10px;
  margin: 6px 0 0;
}
</style>
