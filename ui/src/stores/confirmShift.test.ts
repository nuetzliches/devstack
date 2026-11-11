import { beforeEach, describe, expect, it } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useUi } from './ui';

function click(shiftKey: boolean) {
  document.body.dispatchEvent(new MouseEvent('click', { shiftKey, bubbles: true }));
}

const ask = { title: 'Stop all', text: 'x', danger: true };

describe('confirm with Shift', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
  });

  it('opens the dialog for a plain click', async () => {
    const ui = useUi();
    click(false);
    const p = ui.confirm(ask);
    expect(ui.confirmRequest).not.toBeNull();
    ui.confirmRequest!.resolve(false);
    expect(await p).toBe(false);
  });

  it('skips the dialog after a Shift-click', async () => {
    const ui = useUi();
    click(true);
    expect(await ui.confirm(ask)).toBe(true);
    expect(ui.confirmRequest).toBeNull();
  });

  it('spends the Shift on one dialog only', async () => {
    const ui = useUi();
    click(true);
    await ui.confirm(ask);
    void ui.confirm(ask);
    expect(ui.confirmRequest).not.toBeNull();
  });

  it('keeps a dialog with a choice', () => {
    const ui = useUi();
    click(true);
    void ui.confirm({ ...ask, option: { label: 'Also', checked: false } });
    expect(ui.confirmRequest).not.toBeNull();
  });

  it('keeps a dialog that is not skippable', () => {
    const ui = useUi();
    click(true);
    void ui.confirm({ ...ask, skippable: false });
    expect(ui.confirmRequest).not.toBeNull();
  });

  it('ignores a Shift-click from long ago', () => {
    const ui = useUi();
    click(true);
    const now = Date.now();
    const spy = Date.now;
    Date.now = () => now + 5000;
    try {
      void ui.confirm(ask);
    } finally {
      Date.now = spy;
    }
    expect(ui.confirmRequest).not.toBeNull();
  });
});
