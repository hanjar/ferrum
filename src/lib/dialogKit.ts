/**
 * FER-577 (B0): keyboard kit shared by every modal, radio group and popup menu.
 *
 * Conventions (see also KEYBOARD-AUDIT.md section 2b):
 *  - `use:dialog`  focus on open, Tab trap, focus returns to the opener on close, Esc / Enter
 *    rules, Alt+letter mnemonics, and the `modalDepth` counter that mutes the global shortcuts.
 *  - Enter on a button / link / select / textarea is that control's own business (native
 *    activation). Enter from anywhere else (the dialog itself, a text field, radio, checkbox)
 *    runs the dialog's primary action through `onEnter`.
 *  - Esc cancels (`onEscape`).
 *  - `use:roving`  radio / segmented / tablist: arrows, Home/End, roving tabindex.
 *  - `use:menuKeys` popup menus: arrows, Home/End, accelerator letters, Esc, Tab.
 *  - Mnemonics are written `&Label` (`&&` is a literal ampersand), see `parseMnemonic`, and are
 *    rendered by `Mnemonic.svelte`, which marks the owning control with `data-mnemonic`.
 */

// ── modal depth ────────────────────────────────────────────────────────────────────────────
let depth = 0;

/** Number of open modals (managed by `use:dialog`). Global shortcuts are muted while > 0. */
export function modalDepth(): number {
  return depth;
}

/** Test helper: forget every open modal. */
export function resetModalDepth(): void {
  depth = 0;
}

// ── mnemonic parser ────────────────────────────────────────────────────────────────────────
export interface MnemonicParts {
  /** Text before the access letter. */
  before: string;
  /** The access letter as written (original case), or null when the label has none. */
  key: string | null;
  /** Text after the access letter. */
  after: string;
  /** Label without the `&` markers. */
  label: string;
}

/** `&Simpan` -> `{ before: '', key: 'S', after: 'impan' }`; `Tom && Jerry` keeps a literal `&`. */
export function parseMnemonic(src: string): MnemonicParts {
  let before = '';
  let key: string | null = null;
  let after = '';
  for (let i = 0; i < src.length; i++) {
    const c = src[i];
    const dst = key === null ? 'before' : 'after';
    if (c === '&') {
      const next = src[i + 1];
      if (next === '&') {
        if (dst === 'before') before += '&';
        else after += '&';
        i++;
      } else if (next !== undefined && key === null && /\S/.test(next)) {
        key = next;
        i++;
      }
      continue;
    }
    if (dst === 'before') before += c;
    else after += c;
  }
  return { before, key, after, label: before + (key ?? '') + after };
}

/** Lower-cased access letter of a label (`'&Pack'` -> `'p'`), or null. */
export function mnemonicKey(src: string): string | null {
  return parseMnemonic(src).key?.toLowerCase() ?? null;
}

/**
 * Give every label an access letter that is unused so far (labels that already carry `&` keep
 * theirs). Word initials are preferred, then the remaining letters left to right. Returns the
 * labels with `&` inserted; a label with no free letter is returned unchanged.
 */
export function assignMnemonics(labels: string[], reserved: Iterable<string> = []): string[] {
  const used = new Set<string>();
  for (const r of reserved) used.add(r.toLowerCase());
  for (const l of labels) {
    const k = mnemonicKey(l);
    if (k) used.add(k);
  }
  return labels.map((l) => {
    if (mnemonicKey(l)) return l;
    const text = l;
    const candidates: number[] = [];
    for (let i = 0; i < text.length; i++) {
      if (/[\p{L}\p{N}]/u.test(text[i]) && (i === 0 || !/[\p{L}\p{N}]/u.test(text[i - 1])))
        candidates.push(i);
    }
    for (let i = 0; i < text.length; i++) {
      if (/[\p{L}\p{N}]/u.test(text[i]) && !candidates.includes(i)) candidates.push(i);
    }
    for (const i of candidates) {
      const ch = text[i].toLowerCase();
      if (used.has(ch)) continue;
      used.add(ch);
      return `${text.slice(0, i).replaceAll('&', '&&')}&${text[i]}${text.slice(i + 1).replaceAll('&', '&&')}`;
    }
    return l;
  });
}

/** Access letters used more than once inside `root` (for the unit test of each dialog). */
export function duplicateMnemonics(root: ParentNode): string[] {
  const seen = new Map<string, number>();
  for (const el of root.querySelectorAll<HTMLElement>('[data-mnemonic]')) {
    const k = el.dataset.mnemonic!.toLowerCase();
    seen.set(k, (seen.get(k) ?? 0) + 1);
  }
  return [...seen].filter(([, n]) => n > 1).map(([k]) => k);
}

// ── DOM helpers ────────────────────────────────────────────────────────────────────────────
const TABBABLE =
  'a[href],button,input,select,textarea,summary,[tabindex],[contenteditable="true"],[contenteditable=""]';

function isShown(el: HTMLElement, root: HTMLElement): boolean {
  for (let n: HTMLElement | null = el; n && n !== root.parentElement; n = n.parentElement) {
    if (n.hidden || n.hasAttribute('inert')) return false;
    const st = getComputedStyle(n);
    if (st.display === 'none' || st.visibility === 'hidden') return false;
  }
  return true;
}

/** Elements Tab can land on inside `root`, in DOM order (radio groups count once). */
export function tabbables(root: HTMLElement): HTMLElement[] {
  const all = Array.from(root.querySelectorAll<HTMLElement>(TABBABLE)).filter((el) => {
    if (el === root) return false;
    if ((el as HTMLButtonElement).disabled) return false;
    const ti = el.getAttribute('tabindex');
    if (ti !== null && Number(ti) < 0) return false;
    if (el instanceof HTMLInputElement && el.type === 'hidden') return false;
    return isShown(el, root);
  });
  return all.filter((el) => {
    if (!(el instanceof HTMLInputElement) || el.type !== 'radio' || !el.name) return true;
    const group = all.filter(
      (o): o is HTMLInputElement =>
        o instanceof HTMLInputElement &&
        o.type === 'radio' &&
        o.name === el.name &&
        o.form === el.form,
    );
    const checked = group.find((o) => o.checked);
    return checked ? el === checked : el === group[0];
  });
}

/** Native activation keys: Enter on these must not be hijacked by a dialog. */
function ownsEnter(t: HTMLElement | null, root: HTMLElement): boolean {
  if (!t || t === root) return false;
  if (t.closest('textarea,select')) return true;
  const el = t.closest<HTMLElement>(
    'button,a[href],summary,[role="button"],[role="menuitem"],[role="tab"],[role="option"]',
  );
  return !!el && root.contains(el);
}

/** Click / focus the control a mnemonic points at. */
function activateMnemonic(el: HTMLElement): void {
  let target: HTMLElement | null = el;
  if (el instanceof HTMLLabelElement) target = el.control as HTMLElement | null;
  if (!target) return;
  if ((target as HTMLButtonElement).disabled) return;
  target.focus();
  if (
    target instanceof HTMLButtonElement ||
    (target instanceof HTMLInputElement && ['radio', 'checkbox', 'button'].includes(target.type)) ||
    target.getAttribute('role') === 'tab' ||
    target.getAttribute('role') === 'radio'
  ) {
    target.click();
  }
}

function findMnemonic(root: HTMLElement, key: string): HTMLElement | null {
  const k = key.toLowerCase();
  for (const el of root.querySelectorAll<HTMLElement>('[data-mnemonic]')) {
    if (el.dataset.mnemonic?.toLowerCase() !== k) continue;
    const control = el instanceof HTMLLabelElement ? (el.control as HTMLElement | null) : el;
    if (!control || (control as HTMLButtonElement).disabled) continue;
    if (!isShown(el, root)) continue;
    return el;
  }
  return null;
}

// ── use:dialog ─────────────────────────────────────────────────────────────────────────────
export interface DialogOptions {
  /** Where focus lands on open: a selector, or a function. Default `[data-autofocus]`, else the dialog. */
  initial?: string | (() => HTMLElement | null | undefined);
  /** Esc pressed inside the dialog (cancel). */
  onEscape?: (e: KeyboardEvent) => void;
  /**
   * Enter pressed on the dialog itself or on a text field / radio / checkbox (never on a
   * button, link, select or textarea; Ctrl+Enter also fires from a textarea).
   */
  onEnter?: (e: KeyboardEvent, target: HTMLElement) => void;
  /** Keep Tab inside the dialog. Default true. */
  trap?: boolean;
  /** Alt+letter activates the control carrying `data-mnemonic`. Default true. */
  mnemonics?: boolean;
  /** Hand focus back to the element that was focused before the dialog opened. Default true. */
  returnFocus?: boolean;
}

export function dialog(node: HTMLElement, initialOpts: DialogOptions = {}) {
  let opts = initialOpts;
  const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  function focusInitial() {
    if (node.contains(document.activeElement) && document.activeElement !== node) return;
    const want = opts.initial;
    const el =
      typeof want === 'function'
        ? want()
        : node.querySelector<HTMLElement>(want ?? '[data-autofocus]');
    (el ?? node).focus();
  }
  // FER-577 (N1): count the modal only once the setup did not throw, so a failing `initial`
  // can never leave modalDepth stuck above zero (which would mute every shortcut).
  focusInitial();
  depth += 1;

  function onKeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || e.isComposing) return;
    const t = e.target instanceof HTMLElement ? e.target : null;
    if (e.key === 'Escape' && opts.onEscape) {
      e.preventDefault();
      e.stopPropagation();
      opts.onEscape(e);
    } else if (e.key === 'Enter' && opts.onEnter) {
      const inTextarea = !!t?.closest('textarea');
      if (inTextarea ? !e.ctrlKey : ownsEnter(t, node)) return;
      if (e.altKey || e.shiftKey) return;
      e.preventDefault();
      e.stopPropagation();
      opts.onEnter(e, t ?? node);
    } else if (e.key === 'Tab' && opts.trap !== false) {
      const list = tabbables(node);
      if (list.length === 0) {
        e.preventDefault();
        node.focus();
        return;
      }
      const first = list[0];
      const last = list[list.length - 1];
      const active = document.activeElement;
      const outside = !node.contains(active) || active === node;
      if (active instanceof HTMLElement && !outside && !list.includes(active)) {
        // FER-577 (S1): focus sits on a tabindex=-1 element inside the dialog (an arrow-key
        // target such as an OpsPanel job). The browser would continue after it, possibly
        // behind the modal, so pick the neighbouring tabbable ourselves.
        e.preventDefault();
        const FOLLOWING = Node.DOCUMENT_POSITION_FOLLOWING;
        const next = e.shiftKey
          ? [...list]
              .reverse()
              .find((el) => active.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_PRECEDING)
          : list.find((el) => active.compareDocumentPosition(el) & FOLLOWING);
        (next ?? (e.shiftKey ? last : first)).focus();
      } else if (e.shiftKey && (active === first || outside)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && (active === last || (outside && active !== node))) {
        e.preventDefault();
        first.focus();
      }
    } else if (
      opts.mnemonics !== false &&
      e.altKey &&
      !e.ctrlKey &&
      !e.metaKey &&
      !e.shiftKey &&
      e.key.length === 1
    ) {
      const hit = findMnemonic(node, e.key);
      if (hit) {
        e.preventDefault();
        e.stopPropagation();
        activateMnemonic(hit);
      }
    }
  }
  node.addEventListener('keydown', onKeydown);

  return {
    update(next: DialogOptions) {
      opts = next;
    },
    destroy() {
      node.removeEventListener('keydown', onKeydown);
      depth = Math.max(0, depth - 1);
      if (opts.returnFocus !== false && opener?.isConnected) opener.focus?.();
    },
  };
}

// ── use:roving ─────────────────────────────────────────────────────────────────────────────
export interface RovingOptions {
  /** Items of the group. Default radios, `role=radio` and `role=tab`. */
  items?: string;
  /** Which arrows move: both axes by default. */
  orientation?: 'horizontal' | 'vertical' | 'both';
  /** Select (click) the item that receives focus. Default true. */
  select?: boolean;
}

const ROVING_ITEMS = 'input[type="radio"],[role="radio"],[role="tab"]';

/** Radio / segmented / tablist keyboard: arrows, Home/End, wrap, roving tabindex. */
export function roving(node: HTMLElement, initial: RovingOptions = {}) {
  let opts = initial;

  const items = () =>
    Array.from(node.querySelectorAll<HTMLElement>(opts.items ?? ROVING_ITEMS)).filter(
      (el) => !(el as HTMLButtonElement).disabled && el.getAttribute('aria-disabled') !== 'true',
    );
  const isSelected = (el: HTMLElement) =>
    (el instanceof HTMLInputElement && el.checked) ||
    el.getAttribute('aria-checked') === 'true' ||
    el.getAttribute('aria-selected') === 'true';

  /** Roving tabindex for non-native items; native radios already tab to the checked one. */
  function sync() {
    const list = items();
    const native = list.every((el) => el instanceof HTMLInputElement);
    if (native) return;
    const current = list.find(isSelected) ?? list[0];
    for (const el of list) el.tabIndex = el === current ? 0 : -1;
  }
  sync();
  const mo = new MutationObserver(sync);
  mo.observe(node, {
    subtree: true,
    childList: true,
    attributes: true,
    attributeFilter: ['aria-checked', 'aria-selected', 'checked', 'disabled'],
  });

  function onKeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return;
    const o = opts.orientation ?? 'both';
    let step = 0;
    let edge: 'first' | 'last' | null = null;
    if (e.key === 'ArrowRight' && o !== 'vertical') step = 1;
    else if (e.key === 'ArrowLeft' && o !== 'vertical') step = -1;
    else if (e.key === 'ArrowDown' && o !== 'horizontal') step = 1;
    else if (e.key === 'ArrowUp' && o !== 'horizontal') step = -1;
    else if (e.key === 'Home') edge = 'first';
    else if (e.key === 'End') edge = 'last';
    else return;
    const list = items();
    if (list.length === 0) return;
    const at = list.findIndex((el) => el === e.target || el.contains(e.target as Node));
    if (at < 0) return;
    e.preventDefault();
    e.stopPropagation();
    const next =
      edge === 'first'
        ? list[0]
        : edge === 'last'
          ? list[list.length - 1]
          : list[(at + step + list.length) % list.length];
    next.focus();
    if (opts.select !== false && next !== list[at]) next.click();
    sync();
  }
  node.addEventListener('keydown', onKeydown);

  return {
    update(next: RovingOptions) {
      opts = next ?? {};
      sync();
    },
    destroy() {
      node.removeEventListener('keydown', onKeydown);
      mo.disconnect();
    },
  };
}

// ── use:menuKeys ───────────────────────────────────────────────────────────────────────────
export interface MenuKeysOptions {
  /** Enabled items of this menu level. */
  items?: string;
  /** Esc, or Tab (`reason: 'tab'`, focus is left to move on). */
  onClose?: (reason: 'escape' | 'tab') => void;
  onLeft?: (e: KeyboardEvent) => void;
  onRight?: (e: KeyboardEvent) => void;
}

const MENU_ITEMS = '[role="menuitem"]:not([disabled]):not([aria-disabled="true"])';

/** Popup menu keyboard: arrows (wrap), Home/End, accelerator letters, Esc, Tab. */
export function menuKeys(node: HTMLElement, initial: MenuKeysOptions = {}) {
  let opts = initial;
  const list = () =>
    Array.from(node.querySelectorAll<HTMLElement>(opts.items ?? MENU_ITEMS)).filter(
      (el) => el.closest('[role="menu"]') === node,
    );

  function onKeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || e.isComposing) return;
    const items = list();
    const at = items.indexOf(document.activeElement as HTMLElement);
    const go = (i: number) => {
      if (items.length) items[(i + items.length) % items.length].focus();
    };
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        e.stopPropagation();
        go(at < 0 ? 0 : at + 1);
        return;
      case 'ArrowUp':
        e.preventDefault();
        e.stopPropagation();
        go(at < 0 ? -1 : at - 1);
        return;
      case 'Home':
        e.preventDefault();
        e.stopPropagation();
        go(0);
        return;
      case 'End':
        e.preventDefault();
        e.stopPropagation();
        go(-1);
        return;
      case 'ArrowLeft':
        if (opts.onLeft) {
          e.preventDefault();
          e.stopPropagation();
          opts.onLeft(e);
        }
        return;
      case 'ArrowRight':
        if (opts.onRight) {
          e.preventDefault();
          e.stopPropagation();
          opts.onRight(e);
        }
        return;
      case 'Escape':
        if (opts.onClose) {
          e.preventDefault();
          e.stopPropagation();
          opts.onClose('escape');
        }
        return;
      case 'Tab':
        opts.onClose?.('tab');
        return;
    }
    if (e.key.length !== 1 || e.ctrlKey || e.metaKey || e.altKey || e.key === ' ') return;
    const k = e.key.toLowerCase();
    const marked = items.filter((el) => el.dataset.mnemonic?.toLowerCase() === k);
    if (marked.length === 1) {
      e.preventDefault();
      e.stopPropagation();
      marked[0].focus();
      if (!marked[0].hasAttribute('data-danger')) marked[0].click();
      return;
    }
    const pool =
      marked.length > 1
        ? marked
        : items.filter((el) => el.textContent?.trim().toLowerCase().startsWith(k));
    if (pool.length === 0) return;
    e.preventDefault();
    e.stopPropagation();
    const after = pool.findIndex((el) => items.indexOf(el) > at);
    (pool[after >= 0 ? after : 0] ?? pool[0]).focus();
  }
  node.addEventListener('keydown', onKeydown);

  return {
    update(next: MenuKeysOptions) {
      opts = next ?? {};
    },
    destroy() {
      node.removeEventListener('keydown', onKeydown);
    },
  };
}
