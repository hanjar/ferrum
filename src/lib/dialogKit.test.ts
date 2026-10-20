import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  assignMnemonics,
  dialog,
  duplicateMnemonics,
  menuKeys,
  mnemonicKey,
  modalDepth,
  parseMnemonic,
  resetModalDepth,
  roving,
} from './dialogKit';

afterEach(() => {
  document.body.innerHTML = '';
  resetModalDepth();
});

function html(markup: string): HTMLElement {
  const host = document.createElement('div');
  host.innerHTML = markup;
  document.body.append(host);
  return host;
}

function key(el: Element, k: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const ev = new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true, ...init });
  el.dispatchEvent(ev);
  return ev;
}

describe('parseMnemonic', () => {
  it('splits around the access letter', () => {
    expect(parseMnemonic('&Simpan')).toEqual({
      before: '',
      key: 'S',
      after: 'impan',
      label: 'Simpan',
    });
    expect(parseMnemonic('Sho&w')).toMatchObject({ before: 'Sho', key: 'w', after: '' });
  });
  it('treats && as a literal ampersand and ignores a second marker', () => {
    expect(parseMnemonic('Tom && &Jerry').label).toBe('Tom & Jerry');
    expect(parseMnemonic('Tom && &Jerry').key).toBe('J');
    expect(parseMnemonic('&A&B').key).toBe('A');
    expect(parseMnemonic('plain')).toMatchObject({ key: null, label: 'plain' });
    expect(parseMnemonic('trailing &').key).toBeNull();
  });
  it('mnemonicKey lower-cases', () => {
    expect(mnemonicKey('&7Z')).toBe('7');
    expect(mnemonicKey('&Pack')).toBe('p');
    expect(mnemonicKey('none')).toBeNull();
  });
});

describe('assignMnemonics', () => {
  it('prefers word initials and never repeats a letter', () => {
    const out = assignMnemonics(['Open', 'Open Terminal Here', 'Delete', 'Delete permanently']);
    const keys = out.map((l) => mnemonicKey(l));
    expect(new Set(keys).size).toBe(4);
    expect(keys.every((k) => k !== null)).toBe(true);
    expect(out[0]).toBe('&Open');
  });
  it('keeps explicit markers and honours reserved letters', () => {
    expect(assignMnemonics(['Cut', 'Copy'], ['c'])).toEqual(['C&ut', 'C&opy']);
    expect(assignMnemonics(['&Save', 'Stop'])).toEqual(['&Save', 'S&top']);
  });
});

describe('use:dialog', () => {
  function open(markup: string, opts = {}) {
    const opener = document.createElement('button');
    opener.textContent = 'opener';
    document.body.append(opener);
    opener.focus();
    const host = html(markup);
    const modal = host.firstElementChild as HTMLElement;
    const action = dialog(modal, opts);
    return { opener, modal, action };
  }

  it('focuses the dialog (or [data-autofocus]) and counts toward modalDepth', () => {
    const a = open('<div tabindex="-1"><button>One</button></div>');
    expect(document.activeElement).toBe(a.modal);
    expect(modalDepth()).toBe(1);
    a.action.destroy();
    expect(modalDepth()).toBe(0);
    const b = open(
      '<div tabindex="-1"><button>One</button><button data-autofocus>Two</button></div>',
    );
    expect(document.activeElement?.textContent).toBe('Two');
    b.action.destroy();
  });

  it('honours an `initial` selector', () => {
    const a = open('<div tabindex="-1"><input id="x"><button id="y">b</button></div>', {
      initial: '#y',
    });
    expect(document.activeElement?.id).toBe('y');
    a.action.destroy();
  });

  it('returns focus to the opener on destroy (only if it is still connected)', () => {
    const a = open('<div tabindex="-1"><button>One</button></div>');
    a.action.destroy();
    expect(document.activeElement).toBe(a.opener);
    const b = open('<div tabindex="-1"></div>');
    b.opener.remove();
    expect(() => b.action.destroy()).not.toThrow();
  });

  it('traps Tab and Shift+Tab inside, skipping disabled and hidden controls', () => {
    const a = open(
      `<div tabindex="-1"><button id="first">1</button><button disabled>x</button>
       <button hidden>h</button><button id="last">2</button></div>`,
    );
    const first = a.modal.querySelector<HTMLElement>('#first')!;
    const last = a.modal.querySelector<HTMLElement>('#last')!;
    last.focus();
    expect(key(last, 'Tab').defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(first);
    expect(key(first, 'Tab', { shiftKey: true }).defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(last);
    a.modal.focus();
    expect(key(a.modal, 'Tab', { shiftKey: true }).defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(last);
    a.action.destroy();
  });

  it('counts a radio group once (the checked one) in the trap', () => {
    const a = open(
      `<div tabindex="-1"><input type="radio" name="g" id="r1"><input type="radio" name="g" id="r2" checked>
       <button id="ok">ok</button></div>`,
    );
    const r2 = a.modal.querySelector<HTMLElement>('#r2')!;
    r2.focus();
    key(r2, 'Tab', { shiftKey: true });
    expect(document.activeElement).toBe(a.modal.querySelector('#ok'));
    a.action.destroy();
  });

  it('Escape calls onEscape; Enter on a button is left alone, elsewhere runs onEnter', () => {
    const onEscape = vi.fn();
    const onEnter = vi.fn();
    const a = open(
      '<div tabindex="-1"><input id="i"><textarea id="t"></textarea><button id="b">b</button><select id="s"></select></div>',
      { onEscape, onEnter },
    );
    expect(key(a.modal, 'Escape').defaultPrevented).toBe(true);
    expect(onEscape).toHaveBeenCalledTimes(1);

    expect(key(a.modal.querySelector('#b')!, 'Enter').defaultPrevented).toBe(false);
    expect(key(a.modal.querySelector('#s')!, 'Enter').defaultPrevented).toBe(false);
    expect(key(a.modal.querySelector('#t')!, 'Enter').defaultPrevented).toBe(false);
    expect(onEnter).not.toHaveBeenCalled();

    expect(key(a.modal.querySelector('#i')!, 'Enter').defaultPrevented).toBe(true);
    expect(key(a.modal, 'Enter').defaultPrevented).toBe(true);
    expect(onEnter).toHaveBeenCalledTimes(2);
    key(a.modal.querySelector('#t')!, 'Enter', { ctrlKey: true });
    expect(onEnter).toHaveBeenCalledTimes(3);
    a.action.destroy();
  });

  it('Alt+letter activates the control carrying data-mnemonic and does not leak', () => {
    const leak = vi.fn();
    window.addEventListener('keydown', leak);
    const clicked = vi.fn();
    const a = open(
      `<div tabindex="-1">
         <label data-mnemonic="z"><input type="radio" name="f" id="zip"> ZIP</label>
         <label data-mnemonic="7"><input type="radio" name="f" id="sevenz"> 7Z</label>
         <button data-mnemonic="p" id="pack">Pack</button>
         <button data-mnemonic="x" disabled id="dis">X</button>
         <input data-mnemonic="n" id="name">
       </div>`,
    );
    a.modal.querySelector('#pack')!.addEventListener('click', clicked);
    expect(key(a.modal, '7', { altKey: true }).defaultPrevented).toBe(true);
    expect((a.modal.querySelector('#sevenz') as HTMLInputElement).checked).toBe(true);
    expect(document.activeElement?.id).toBe('sevenz');
    key(a.modal, 'P', { altKey: true });
    expect(clicked).toHaveBeenCalledTimes(1);
    key(a.modal, 'n', { altKey: true });
    expect(document.activeElement?.id).toBe('name');
    expect(leak).not.toHaveBeenCalled();
    key(a.modal, 'x', { altKey: true }); // disabled target: no match, left alone
    expect(document.activeElement?.id).toBe('name');
    expect(key(a.modal, 'p', { altKey: true, ctrlKey: true }).defaultPrevented).toBe(false);
    window.removeEventListener('keydown', leak);
    a.action.destroy();
  });

  it('nested dialogs stack modalDepth', () => {
    const a = open('<div tabindex="-1"></div>');
    const b = open('<div tabindex="-1"></div>');
    expect(modalDepth()).toBe(2);
    b.action.destroy();
    expect(modalDepth()).toBe(1);
    a.action.destroy();
    expect(modalDepth()).toBe(0);
  });
});

describe('duplicateMnemonics', () => {
  it('reports a letter used twice', () => {
    const host = html(
      '<button data-mnemonic="a"></button><button data-mnemonic="A"></button><i data-mnemonic="b"></i>',
    );
    expect(duplicateMnemonics(host)).toEqual(['a']);
  });
});

describe('use:roving', () => {
  const group = () =>
    html(
      `<div role="tablist">
         <button role="tab" aria-selected="true" id="t1">1</button>
         <button role="tab" aria-selected="false" id="t2">2</button>
         <button role="tab" aria-selected="false" id="t3" disabled>3</button>
         <button role="tab" aria-selected="false" id="t4">4</button>
       </div>`,
    ).firstElementChild as HTMLElement;

  it('keeps one tab stop on the selected item', () => {
    const g = group();
    const r = roving(g);
    expect(g.querySelector<HTMLElement>('#t1')!.tabIndex).toBe(0);
    expect(g.querySelector<HTMLElement>('#t2')!.tabIndex).toBe(-1);
    r.destroy();
  });

  it('arrows wrap, skip disabled, Home/End jump, and select the focused item', async () => {
    const g = group();
    const r = roving(g);
    g.addEventListener('click', (e) => {
      const el = e.target as HTMLElement;
      g.querySelectorAll('[role=tab]').forEach((t) =>
        t.setAttribute('aria-selected', String(t === el)),
      );
    });
    const t1 = g.querySelector<HTMLElement>('#t1')!;
    t1.focus();
    key(t1, 'ArrowRight');
    expect(document.activeElement?.id).toBe('t2');
    key(document.activeElement!, 'ArrowRight');
    expect(document.activeElement?.id).toBe('t4');
    key(document.activeElement!, 'ArrowRight');
    expect(document.activeElement?.id).toBe('t1');
    key(document.activeElement!, 'ArrowLeft');
    expect(document.activeElement?.id).toBe('t4');
    key(document.activeElement!, 'Home');
    expect(document.activeElement?.id).toBe('t1');
    key(document.activeElement!, 'End');
    expect(document.activeElement?.id).toBe('t4');
    await Promise.resolve();
    expect(g.querySelector<HTMLElement>('#t4')!.tabIndex).toBe(0);
    expect(g.querySelector<HTMLElement>('#t1')!.tabIndex).toBe(-1);
    r.destroy();
  });

  it('works on native radios and honours orientation', () => {
    const g = html(
      '<div><input type="radio" name="a" id="a1" checked><input type="radio" name="a" id="a2"></div>',
    ).firstElementChild as HTMLElement;
    const r = roving(g, { orientation: 'horizontal' });
    const a1 = g.querySelector<HTMLInputElement>('#a1')!;
    a1.focus();
    expect(key(a1, 'ArrowDown').defaultPrevented).toBe(false);
    key(a1, 'ArrowRight');
    expect(g.querySelector<HTMLInputElement>('#a2')!.checked).toBe(true);
    r.destroy();
  });
});

describe('use:menuKeys', () => {
  const menu = () =>
    html(
      `<div role="menu">
         <button role="menuitem" id="m1" data-mnemonic="o">Open</button>
         <button role="menuitem" id="m2" disabled>Disabled</button>
         <button role="menuitem" id="m3">Delete</button>
         <button role="menuitem" id="m4">Details</button>
       </div>`,
    ).firstElementChild as HTMLElement;

  it('arrows wrap over enabled items; Home/End jump', () => {
    const m = menu();
    const k = menuKeys(m);
    m.querySelector<HTMLElement>('#m1')!.focus();
    key(document.activeElement!, 'ArrowDown');
    expect(document.activeElement?.id).toBe('m3');
    key(document.activeElement!, 'ArrowUp');
    expect(document.activeElement?.id).toBe('m1');
    key(document.activeElement!, 'ArrowUp');
    expect(document.activeElement?.id).toBe('m4');
    key(document.activeElement!, 'Home');
    expect(document.activeElement?.id).toBe('m1');
    key(document.activeElement!, 'End');
    expect(document.activeElement?.id).toBe('m4');
    k.destroy();
  });

  it('a unique accelerator letter activates; a shared first letter cycles', () => {
    const m = menu();
    const k = menuKeys(m);
    const opened = vi.fn();
    m.querySelector('#m1')!.addEventListener('click', opened);
    m.querySelector<HTMLElement>('#m3')!.focus();
    key(document.activeElement!, 'o');
    expect(opened).toHaveBeenCalledTimes(1);
    key(document.activeElement!, 'd');
    expect(document.activeElement?.id).toBe('m3');
    key(document.activeElement!, 'd');
    expect(document.activeElement?.id).toBe('m4');
    key(document.activeElement!, 'd');
    expect(document.activeElement?.id).toBe('m3');
    k.destroy();
  });

  it('Escape and Tab report to onClose; Left/Right only when wired', () => {
    const m = menu();
    const onClose = vi.fn();
    const onRight = vi.fn();
    const k = menuKeys(m, { onClose, onRight });
    m.querySelector<HTMLElement>('#m1')!.focus();
    expect(key(document.activeElement!, 'Escape').defaultPrevented).toBe(true);
    expect(onClose).toHaveBeenLastCalledWith('escape');
    expect(key(document.activeElement!, 'Tab').defaultPrevented).toBe(false);
    expect(onClose).toHaveBeenLastCalledWith('tab');
    key(document.activeElement!, 'ArrowRight');
    expect(onRight).toHaveBeenCalledTimes(1);
    expect(key(document.activeElement!, 'ArrowLeft').defaultPrevented).toBe(false);
    k.destroy();
  });
});
