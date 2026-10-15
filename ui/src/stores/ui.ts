import { defineStore } from 'pinia';
import { computed, ref, watchEffect } from 'vue';

export type ThemePref = 'system' | 'light' | 'dark';
export type ToastKind = 'success' | 'error' | 'info' | 'warning';
export type Toast = { id: number; kind: ToastKind; title: string; message?: string };

export type ConfirmRequest = {
  title: string;
  text: string;
  confirmLabel: string;
  danger: boolean;
  resolve(ok: boolean): void;
};

export type ProblemRequest = { title: string; message: string; hint?: string };

const THEME_KEY = 'devstack.theme';

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
  function confirm(opts: { title: string; text: string; confirmLabel?: string; danger?: boolean }): Promise<boolean> {
    confirmRequest.value?.resolve(false);
    return new Promise((resolve) => {
      confirmRequest.value = {
        title: opts.title,
        text: opts.text,
        confirmLabel: opts.confirmLabel ?? 'Continue',
        danger: opts.danger ?? false,
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

  return { theme, effectiveTheme, cycleTheme, toasts, toast, dismiss, confirmRequest, confirm, problem, showProblem, pendingOpen };
});
