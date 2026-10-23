import { defineStore } from 'pinia';
import { computed, ref, watchEffect } from 'vue';
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
  function confirm(opts: { title: string; text: string; confirmLabel?: string; danger?: boolean; option?: ConfirmOption }): Promise<boolean> {
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

  return {
    theme,
    effectiveTheme,
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
