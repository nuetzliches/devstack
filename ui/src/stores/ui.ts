import { defineStore } from 'pinia';
import { computed, ref, watch, watchEffect } from 'vue';
import { persist } from '@/lib/persist';

export type ThemePref = 'system' | 'light' | 'dark';
export type ToastKind = 'success' | 'error' | 'info' | 'warning';
export type Toast = { id: number; kind: ToastKind; title: string; message?: string };

export type ConfirmRequest = {
  title: string;
  text: string;
  confirmLabel: string;
  danger: boolean;
  /** A choice that goes with it; the dialog sets `checked`. */
  option?: ConfirmOption;
  resolve(ok: boolean): void;
};

export type ConfirmOption = { label: string; hint?: string; checked: boolean };

export type ProblemRequest = { title: string; message: string; hint?: string };

const THEME_KEY = 'devstack.theme';

/** Below this width the sidebar is a drawer (keep in step with App.vue and TopBar.vue). */
export const SIDEBAR_BREAKPOINT = 1000;
export const SIDEBAR_DEFAULT = 248;
export const SIDEBAR_MIN = 180;
export const SIDEBAR_MAX = 520;

function readTheme(): ThemePref {
  try {
    const t = localStorage.getItem(THEME_KEY);
    return t === 'light' || t === 'dark' ? t : 'system';
  } catch {
    return 'system';
  }
}

export const useUi = defineStore('ui', () => {
  // ---------------------------------------------------------- theme
  const theme = ref<ThemePref>(readTheme());
  const media = window.matchMedia('(prefers-color-scheme: dark)');
  const systemDark = ref(media.matches);
  media.addEventListener('change', (e) => (systemDark.value = e.matches));
  const effectiveTheme = computed(() => (theme.value === 'system' ? (systemDark.value ? 'dark' : 'light') : theme.value));

  watchEffect(() => {
    const root = document.documentElement;
    if (theme.value === 'system') delete root.dataset.theme;
    else root.dataset.theme = theme.value;
    try {
      if (theme.value === 'system') localStorage.removeItem(THEME_KEY);
      else localStorage.setItem(THEME_KEY, theme.value);
    } catch {
      /* ignore */
    }
  });

  function cycleTheme() {
    theme.value = theme.value === 'system' ? 'light' : theme.value === 'light' ? 'dark' : 'system';
  }

  // ---------------------------------------------------------- toasts
  const toasts = ref<Toast[]>([]);
  let nextToast = 1;
  function toast(kind: ToastKind, title: string, message?: string, ms = kind === 'error' ? 8000 : 4000) {
    const id = nextToast++;
    toasts.value = [...toasts.value.slice(-4), { id, kind, title, message }];
    setTimeout(() => dismiss(id), ms);
  }
  function dismiss(id: number) {
    toasts.value = toasts.value.filter((t) => t.id !== id);
  }

  // ---------------------------------------------------------- dialogs
  const confirmRequest = ref<ConfirmRequest | null>(null);

  // Shift on the click (or Enter) that started an action skips its
  // confirmation. The action asks after the event, so the last Shift is
  // remembered for a moment and spent by the first dialog that uses it.
  let shiftAt = 0;
  const SHIFT_WINDOW_MS = 1000;
  function noteShift(e: MouseEvent | KeyboardEvent) {
    if (e instanceof KeyboardEvent && e.key !== 'Enter' && e.key !== ' ') return;
    shiftAt = e.shiftKey ? Date.now() : 0;
  }
  document.addEventListener('click', noteShift, true);
  document.addEventListener('keydown', noteShift, true);

  function confirm(opts: {
    title: string;
    text: string;
    confirmLabel?: string;
    danger?: boolean;
    option?: ConfirmOption;
    /** False for a dialog Shift must not skip: it loses data or logins, or the daemon asked for it. */
    skippable?: boolean;
  }): Promise<boolean> {
    const skip = shiftAt > 0 && Date.now() - shiftAt < SHIFT_WINDOW_MS;
    shiftAt = 0;
    if (skip && !opts.option && opts.skippable !== false) return Promise.resolve(true);
    confirmRequest.value?.resolve(false);
    return new Promise((resolve) => {
      confirmRequest.value = {
        title: opts.title,
        text: opts.text,
        confirmLabel: opts.confirmLabel ?? 'Continue',
        danger: opts.danger ?? false,
        option: opts.option,
        resolve: (ok) => {
          confirmRequest.value = null;
          resolve(ok);
        },
      };
    });
  }

  const problem = ref<ProblemRequest | null>(null);
  function showProblem(p: ProblemRequest) {
    problem.value = p;
  }

  const pendingOpen = ref(false);

  // ---------------------------------------------------------- sidebar
  // On a wide screen the sidebar sits beside the page and stays hidden or
  // shown as it was left; on a narrow one it is a drawer over the page that
  // opens on request and closes again (App.vue). Its width is dragged and
  // kept for both.
  const wideQuery = window.matchMedia(`(min-width: ${SIDEBAR_BREAKPOINT}px)`);
  const wide = ref(wideQuery.matches);
  wideQuery.addEventListener('change', (e) => {
    wide.value = e.matches;
    drawerOpen.value = false;
  });
  const sidebarOpen = ref(persist.get<boolean>('sidebar.open') ?? true);
  const drawerOpen = ref(false);
  const sidebarShown = computed(() => (wide.value ? sidebarOpen.value : drawerOpen.value));
  function toggleSidebar() {
    wide.value = wideQuery.matches;
    if (wide.value) {
      sidebarOpen.value = !sidebarOpen.value;
      persist.set('sidebar.open', sidebarOpen.value);
    } else drawerOpen.value = !drawerOpen.value;
  }
  const clampWidth = (px: number) => Math.round(Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, px)));
  const sidebarWidth = ref(clampWidth(persist.get<number>('sidebar.width') ?? SIDEBAR_DEFAULT));
  /** While dragging, `save` is false: only the last width is kept. */
  function setSidebarWidth(px: number, save = true) {
    sidebarWidth.value = clampWidth(px);
    if (save) persist.set('sidebar.width', sidebarWidth.value);
  }

  // ---------------------------------------------------------- sidebar filter
  // The sidebar's filter is kept here and in this tab's session storage, not in
  // the component: the sidebar is mounted again while the hub loads, and the
  // page reloads when the hub brings a new shell (app-update.ts); neither must
  // empty what was typed.
  const filterOpen = ref(persist.getSession<boolean>('sidebar.filter.open') ?? false);
  const filterQuery = ref(persist.getSession<string>('sidebar.filter.query') ?? '');
  watch([filterOpen, filterQuery], ([open, query]) => {
    persist.setSession('sidebar.filter.open', open);
    persist.setSession('sidebar.filter.query', query);
  });

  return {
    theme,
    effectiveTheme,
    filterOpen,
    filterQuery,
    cycleTheme,
    toasts,
    toast,
    dismiss,
    confirmRequest,
    confirm,
    problem,
    showProblem,
    pendingOpen,
    wide,
    sidebarOpen,
    drawerOpen,
    sidebarShown,
    toggleSidebar,
    sidebarWidth,
    setSidebarWidth,
  };
});
