<script setup lang="ts">
// A tree's path as a menu (ADR-0046): copy it, or open the tree with a
// program the hub found. Used by a lane's header and by the stack's page.
// Where nothing can be opened from here (`canOpen` false: the hub away, a
// lane shared with us on another machine), it offers the copy only. The
// favourite programs (chosen in its "Manage programs…") get a button of
// their own beside it.
import { computed, ref, watchEffect } from 'vue';
import type { Opener } from '@/api/types';
import { usePopover } from '@/lib/popover';
import { useOpenerPrefs } from '@/lib/openers';
import { useHub } from '@/stores/hub';
import { useUi } from '@/stores/ui';
import Icon from './icons/Icon.vue';
import OpenersDialog from './OpenersDialog.vue';

const props = defineProps<{
  path: string;
  stackId: string | null;
  canOpen: boolean;
}>();

const hub = useHub();
const ui = useUi();
const root = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const { open, close } = usePopover(root, trigger);
const openerPrefs = useOpenerPrefs();
const openers = computed(() => (props.canOpen ? openerPrefs.shown(hub.openers ?? []) : []));
const favs = computed(() => (props.canOpen ? openerPrefs.favorites(hub.openers ?? []) : []));
const isFav = (o: Opener) => openerPrefs.prefs.value.favorites.includes(o.id);
// The programs are read when the menu opens; with favourites, at once, so
// their buttons are there without it (the store reads them once).
watchEffect(() => {
  if (props.canOpen && openerPrefs.prefs.value.favorites.length) void hub.loadOpeners();
});
const managing = ref(false);
const openerIcon = {
  files: 'folder',
  terminal: 'terminal',
  editor: 'file',
  git: 'gitBranch',
  agent: 'zap',
} as const;

/** The path's folder and its last part: a long path is cut in the folder, the tree's name stays. */
const parts = computed(() => {
  const m = /^(.*[\\/])([^\\/]+)[\\/]?$/.exec(props.path);
  return m ? { dir: m[1], leaf: m[2] } : { dir: '', leaf: props.path };
});

function toggle() {
  open.value = !open.value;
  if (open.value && props.canOpen) void hub.loadOpeners();
}
function manage() {
  close();
  managing.value = true;
}
async function copyPath() {
  close();
  try {
    await navigator.clipboard.writeText(props.path);
    ui.toast('info', 'Copied', props.path);
  } catch {
    ui.toast('info', props.path, 'Copying is not allowed here.');
  }
}
function openWith(o: Opener) {
  close();
  if (props.stackId) void hub.openWith(props.stackId, props.path, o);
}
</script>

<template>
  <div class="path-menu">
    <div ref="root" class="picker path-picker">
      <button ref="trigger" type="button" class="path-btn mono" :aria-expanded="open" aria-haspopup="menu" :title="`${path}: copy, or open in another program`" data-testid="path-menu" @click="toggle">
        <Icon name="folder" :size="13" class="muted" />
        <span class="where"
          ><span class="dir">{{ parts.dir }}</span
          ><span class="leaf">{{ parts.leaf }}</span></span
        >
        <Icon name="chevronDown" :size="13" class="muted" />
      </button>
      <div v-if="open" class="menu" role="menu" aria-label="Open the folder" data-testid="path-menu-list">
        <button type="button" class="opt" role="menuitem" data-testid="path-copy" @click="copyPath"><Icon name="copy" :size="14" class="muted" /><span class="grow">Copy path</span></button>
        <template v-if="canOpen">
          <div class="sep" />
          <span v-if="!hub.openers" class="faint small menu-note"><span class="spinner" /> Looking for programs…</span>
          <span v-else-if="!hub.openers.length" class="faint small menu-note">No program to open it with on this machine.</span>
          <span v-else-if="!openers.length" class="faint small menu-note">All programs are hidden.</span>
          <button
            v-for="(o, i) in openers"
            :key="o.id"
            type="button"
            class="opt"
            :class="{ 'group-start': i > 0 && openers[i - 1].kind !== o.kind }"
            role="menuitem"
            data-testid="path-open"
            @click="openWith(o)"
          >
            <Icon :name="openerIcon[o.kind] ?? 'external'" :size="14" class="muted" /><span class="grow">{{ o.label }}</span>
            <Icon v-if="isFav(o)" name="star" :size="11" class="faint" label="Also beside the path" />
          </button>
          <div v-if="hub.openers?.length" class="menu-foot">
            <button type="button" class="link-btn" role="menuitem" data-testid="path-manage" @click="manage">Manage programs…</button>
          </div>
        </template>
      </div>
      <OpenersDialog :open="managing" :openers="hub.openers ?? []" @close="managing = false" />
    </div>
    <button
      v-for="o in favs"
      :key="o.id"
      type="button"
      class="fav-btn"
      :title="`Open in ${o.label}`"
      :aria-label="`Open in ${o.label}`"
      data-testid="path-favorite"
      :data-id="o.id"
      @click="openWith(o)"
    >
      <Icon :name="openerIcon[o.kind] ?? 'external'" :size="14" />
    </button>
  </div>
</template>

<style scoped>
.path-menu {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}
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
.picker {
  position: relative;
}
.path-picker {
  min-width: 0;
  max-width: 48ch;
}
.path-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 100%;
  padding: 3px 8px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: var(--surface-2);
  color: var(--text-2);
  font-size: 12.5px;
  cursor: pointer;
}
.path-btn:hover,
.path-btn[aria-expanded='true'] {
  border-color: var(--border-strong);
  color: var(--text);
}
.where {
  display: flex;
  min-width: 0;
}
.path-btn .dir {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.path-btn .leaf {
  flex: none;
  max-width: 24ch;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
  font-weight: 600;
}
.menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  z-index: 40;
  min-width: 220px;
  max-height: 60vh;
  overflow: auto;
  background: var(--bg-elev);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-lg);
  padding: 6px;
  display: flex;
  flex-direction: column;
}
.opt {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 8px;
  border: 0;
  border-radius: 8px;
  background: transparent;
  color: inherit;
  font: inherit;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
}
.opt:hover {
  background: var(--surface-hover);
}
.grow {
  flex: 1;
  min-width: 0;
}
.sep {
  height: 1px;
  margin: 4px 2px;
  background: var(--border);
}
.group-start {
  margin-top: 5px;
  position: relative;
}
.group-start::before {
  content: '';
  position: absolute;
  top: -3px;
  left: 8px;
  right: 8px;
  height: 1px;
  background: var(--border);
  opacity: 0.6;
}
.menu-foot {
  margin: 4px -6px -6px;
  padding: 7px 14px;
  border-top: 1px solid var(--border);
}
.link-btn {
  border: 0;
  background: none;
  padding: 0;
  color: var(--accent);
  font: inherit;
  font-size: 12px;
  cursor: pointer;
}
.link-btn:hover {
  text-decoration: underline;
}
.menu-note {
  padding: 6px 8px;
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.small {
  font-size: 12px;
}
</style>
