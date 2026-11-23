<script setup lang="ts">
// The favourite programs (chosen in "Manage programs…", ADR-0046) as round
// buttons that open `path`, a tree of stack `stackId`: beside a tree's path
// (PathMenu) and on a stack's tile on the start page. The programs are read
// once per page, at once when there are favourites.
import { computed, watchEffect } from 'vue';
import type { Opener } from '@/api/types';
import { useOpenerPrefs } from '@/lib/openers';
import { useHub } from '@/stores/hub';
import OpenerIcon from './OpenerIcon.vue';

const props = defineProps<{ stackId: string | null; path: string }>();
/** `opened`: a program was asked to open the path (PathMenu closes its menu). */
const emit = defineEmits<{ opened: [] }>();

const hub = useHub();
const openerPrefs = useOpenerPrefs();
const favs = computed(() => openerPrefs.favorites(hub.openers ?? []));
watchEffect(() => {
  if (openerPrefs.prefs.value.favorites.length) void hub.loadOpeners();
});

function openWith(o: Opener) {
  emit('opened');
  if (props.stackId) void hub.openWith(props.stackId, props.path, o);
}
</script>

<template>
  <button
    v-for="o in favs"
    :key="o.id"
    type="button"
    class="fav-btn"
    :title="`Open in ${o.label}`"
    :aria-label="`Open in ${o.label}`"
    data-testid="path-favorite"
    :data-id="o.id"
    @click.stop.prevent="openWith(o)"
  >
    <OpenerIcon :opener="o" />
  </button>
</template>

<style scoped>
/* A favourite: a round button in the path pill's look. */
.fav-btn {
  display: grid;
  place-items: center;
  width: 26px;
  height: 26px;
  flex: none;
  padding: 0;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: var(--surface-2);
  color: var(--text-2);
  cursor: pointer;
}
.fav-btn:hover {
  border-color: var(--border-strong);
  color: var(--text);
}
</style>
