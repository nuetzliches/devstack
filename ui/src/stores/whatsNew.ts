import { defineStore } from 'pinia';
import { computed, ref, watch } from 'vue';
import { laterVersion, newestUpTo, sectionsBetween, type ChangelogSection } from '@/lib/changelog';
import { changelog } from '@/lib/changelogText';
import { persist } from '@/lib/persist';
import { useHub } from '@/stores/hub';

/** localStorage: the devstack version whose news this browser has shown. */
const SEEN = 'whatsNewSeen';
/** Most releases the view lists after an update; the rest is in the changelog itself. */
export const MAX_RELEASES = 10;
/** Releases it lists when opened without an update behind it. */
const LATEST = 3;

/**
 * "What's new": after the hub runs a newer devstack than this browser last
 * saw, whichever way it was installed (the dashboard's button, `devstack
 * update`, winget, Homebrew), a line says so and opens the changelog's
 * sections in between. The first visit of a browser only notes the version.
 * Not the stack file's "upgrade" banner: that one is about a stack file.
 */
export const useWhatsNew = defineStore('whatsNew', () => {
  const hub = useHub();
  /** The changelog's releases; a ref so a test can put another in. */
  const releases = ref<ChangelogSection[]>(changelog);
  const seen = ref<string | null>(persist.get<string>(SEEN));
  const open = ref(false);
  /** What the open view lists, fixed when it opens. */
  const shown = ref<ChangelogSection[]>([]);
  /** Releases the view leaves out (more than `MAX_RELEASES` behind). */
  const omitted = ref(0);

  /** The hub's version, once the hub answered (not the one remembered from the last visit). */
  const current = computed(() => (hub.status === 'ok' ? hub.version : null));
  /** The releases after the one seen, up to the installed one. */
  const pending = computed(() => (seen.value && current.value ? sectionsBetween(releases.value, seen.value, current.value) : []));
  const banner = computed(() => !open.value && pending.value.length > 0);

  function remember(v: string) {
    seen.value = v;
    persist.set(SEEN, v);
  }

  /** A version is installed without news to show (first visit, a release without an entry, a rollback): note it. */
  function sync() {
    const now = current.value;
    if (!now) return;
    if (seen.value === null || laterVersion(seen.value, now) || (seen.value !== now && !pending.value.length)) remember(now);
  }
  watch(current, sync, { immediate: true });

  function show() {
    const now = current.value;
    if (!now) return;
    const list = pending.value.length ? pending.value : newestUpTo(releases.value, now, LATEST);
    shown.value = list.slice(0, MAX_RELEASES);
    omitted.value = list.length - shown.value.length;
    open.value = true;
  }

  /** Closing the view, or the line's cross, is "seen". */
  function dismiss() {
    open.value = false;
    if (current.value) remember(current.value);
  }

  return { seen, current, pending, banner, open, shown, omitted, releases, show, dismiss };
});
