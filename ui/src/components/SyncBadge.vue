<script setup lang="ts">
// How far a tree's branch is ahead of (↑) and behind (↓) its upstream, as the
// local refs say: devstack asks no remote, a fetch by any git client moves it.
// A branch that is ahead and not behind can be pushed: a click asks, then the
// hub runs `git push` to the branch's own upstream. Behind it (or both), the
// page cannot push; that is for a git client. The start page's tiles and
// rows and the lane's header show it the same way (ADR-0088, ADR-0117).
import { computed } from 'vue';
import { useBusy } from '@/lib/actions';
import { plural, pushable as canPush, pushTree } from '@/lib/gitPush';

const props = defineProps<{
  stackId: string;
  /** The tree whose branch it is: the stack's folder or a lane's. */
  path: string;
  branch: string;
  upstream: string;
  ahead: number;
  behind: number;
  /** A tree that is not ours to change (a lane shared with us). */
  readOnly?: boolean;
  /** Where several stacks are in view: named in the question and the answer. */
  stackName?: string;
}>();

const { busy, run } = useBusy();
const pushable = computed(() => canPush(props) && !props.readOnly);
const title = computed(() => {
  const parts = [props.ahead && `${plural(props.ahead)} not on ${props.upstream}`, props.behind && `${props.behind} on ${props.upstream} not here`].filter(Boolean);
  const how = pushable.value
    ? `Push ${props.branch} to ${props.upstream}`
    : props.behind
      ? 'Bring it up to date in a git client'
      : '';
  return `${parts.join(', ')}, as of the last fetch.${how ? ` ${how}` : ''}`;
});

function push() {
  void run('push', () => pushTree(props.stackId, props.path, props, props.stackName));
}
</script>

<template>
  <button v-if="pushable" type="button" class="sync-btn mono" :class="{ busy }" :disabled="!!busy" :title="title" data-testid="branch-sync" @click.stop="push">
    <span class="spinner" v-if="busy" />
    <span data-testid="branch-ahead">↑{{ ahead }}</span>
  </button>
  <span v-else class="sync-btn static mono" :title="title" data-testid="branch-sync">
    <span v-if="ahead" data-testid="branch-ahead">↑{{ ahead }}</span>
    <span v-if="behind" data-testid="branch-behind">↓{{ behind }}</span>
  </span>
</template>

<style scoped>
.sync-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 3px 8px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: var(--surface-2);
  color: var(--text-2);
  font-size: 12px;
  line-height: 1.2;
  cursor: pointer;
}
button.sync-btn:hover:not(:disabled) {
  border-color: var(--border-strong);
  color: var(--text);
}
button.sync-btn:disabled {
  cursor: progress;
}
.sync-btn.static {
  cursor: default;
}
</style>
