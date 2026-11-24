<script setup lang="ts">
// "Add a stack" for a repository not on this machine yet: the Git clients of
// the path menu (as arranged in "Manage programs…") start to clone it, with an
// https address in their clone dialog (ADR-0046, amendment 2026-11-24). Where
// the clone goes, the user chooses there and pastes the folder afterwards.
import { computed, onMounted, ref } from 'vue';
import type { Opener } from '@/api/types';
import { useOpenerPrefs } from '@/lib/openers';
import { useHub } from '@/stores/hub';
import OpenerIcon from './OpenerIcon.vue';
import OpenersDialog from './OpenersDialog.vue';

const props = defineProps<{ url?: string }>();

const hub = useHub();
const openerPrefs = useOpenerPrefs();
const managing = ref(false);
onMounted(() => void hub.loadOpeners());

const found = computed(() => (hub.openers ?? []).filter((o) => o.kind === 'git'));
const shown = computed(() => openerPrefs.shown(hub.openers ?? []).filter((o) => o.kind === 'git'));

function clone(o: Opener) {
  void hub.cloneWith(o, props.url);
}
</script>

<template>
  <div v-if="found.length" class="clone" data-testid="clone-with">
    <span class="muted">{{ url ? 'Clone it first with' : 'Not cloned yet? Clone it with' }}</span>
    <button v-for="o in shown" :key="o.id" type="button" class="btn btn-sm" data-testid="clone-with-open" :data-id="o.id" @click="clone(o)">
      <OpenerIcon :opener="o" /> {{ o.label }}
    </button>
    <span v-if="!shown.length" class="faint">a Git client, unchecked in</span>
    <button type="button" class="link-btn" data-testid="clone-with-manage" @click="managing = true">Manage programs…</button>
    <p v-if="url" class="muted small hint">Its clone dialog opens with this address; then paste the folder it was cloned to.</p>
    <OpenersDialog :open="managing" :openers="hub.openers ?? []" @close="managing = false" />
  </div>
</template>

<style scoped>
.clone {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px 8px;
  margin: -4px 0 8px;
  font-size: 13px;
}
.link-btn {
  border: 0;
  background: none;
  padding: 0;
  color: var(--accent);
  font: inherit;
  cursor: pointer;
}
.link-btn:hover {
  text-decoration: underline;
}
.hint {
  flex-basis: 100%;
  margin: 0;
}
</style>
