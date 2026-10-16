// Tooltips for every `title` in the app, drawn by the page instead of the
// browser's native ones: they look like the rest of the dashboard, appear
// sooner, and keep long texts (an error, a path) readable. Any element with
// a `title` gets one; no component has to know. On first hover the text
// moves to `data-tip`, so the browser never shows its own; an element that
// has no other accessible name keeps it as `aria-label`.

const DELAY = 350;

export function installTooltips(): void {
  const tip = document.createElement('div');
  tip.className = 'app-tooltip';
  tip.setAttribute('role', 'tooltip');
  tip.hidden = true;
  document.body.appendChild(tip);

  let current: HTMLElement | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;

  /** The tooltip text of `el`, taking over a `title` Vue (re)set since. */
  function textOf(el: HTMLElement): string {
    const t = el.getAttribute('title');
    if (t !== null) {
      el.removeAttribute('title');
      if (t) el.dataset.tip = t;
      else delete el.dataset.tip;
      if (t && !el.getAttribute('aria-label') && !el.textContent?.trim()) el.setAttribute('aria-label', t);
    }
    return el.dataset.tip ?? '';
  }

  function hide() {
    if (timer) clearTimeout(timer);
    timer = undefined;
    current = null;
    tip.hidden = true;
  }

  function place(el: HTMLElement) {
    const r = el.getBoundingClientRect();
    tip.hidden = false;
    const t = tip.getBoundingClientRect();
    const gap = 8;
    let top = r.top - t.height - gap;
    if (top < 4) top = r.bottom + gap;
    let left = r.left + r.width / 2 - t.width / 2;
    left = Math.max(6, Math.min(left, window.innerWidth - t.width - 6));
    tip.style.top = `${Math.round(top + window.scrollY)}px`;
    tip.style.left = `${Math.round(left + window.scrollX)}px`;
  }

  function show(el: HTMLElement, now = false) {
    const text = textOf(el);
    if (timer) clearTimeout(timer);
    if (!text) return hide();
    current = el;
    const open = () => {
      if (current !== el || !el.isConnected) return;
      tip.textContent = text;
      place(el);
    };
    if (now || !tip.hidden) open();
    else timer = setTimeout(open, DELAY);
  }

  const targetOf = (e: Event) => (e.target instanceof Element ? (e.target.closest('[title], [data-tip]') as HTMLElement | null) : null);

  document.addEventListener('mouseover', (e) => {
    const el = targetOf(e);
    if (el === current) return;
    if (el) show(el);
    else hide();
  });
  document.addEventListener('focusin', (e) => {
    const el = targetOf(e);
    if (el && (e.target as Element).matches(':focus-visible')) show(el, true);
  });
  document.addEventListener('focusout', hide);
  document.addEventListener('mousedown', hide, true);
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') hide();
  });
  window.addEventListener('scroll', hide, true);
  window.addEventListener('blur', hide);
}
