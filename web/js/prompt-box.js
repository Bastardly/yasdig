// The prompt input, as one element used six times: a prompt and a negative on
// each of the three tabs that ask for one. It is where the features go, and the
// first of them is below — Ctrl+↑/↓ and Ctrl+wheel to weight a word, once here
// rather than six times across three tab modules.
//
// It *wraps* its control rather than replacing it. The <textarea> or <input>
// inside keeps the id, so `$('prompt').value`, `<label for>`, wireHints'
// `label.htmlFor` lookup and every existing listener go on working untouched —
// an element that owned the value itself would have to forward all of that, and
// would break the first one it forgot.
//
// Light DOM, no shadow root: `style.css` is one global sheet (see CLAUDE.md), and
// a shadow root would need its own copy of the design system to style one
// textarea.
import { adjust, STEP } from './prompt-weight.js';

class PromptBox extends HTMLElement {
  #wired = false;

  // Where the text actually lives. A getter rather than something cached in
  // connectedCallback, so the element is correct however the parser and the
  // upgrade happen to interleave, and re-parenting it needs no bookkeeping.
  get field() {
    return this.querySelector('textarea, input');
  }

  get value() {
    return this.field.value;
  }

  set value(v) {
    this.field.value = v;
  }

  // The listeners go on the element, not on the field, and both events bubble
  // out of it — so this keeps working if the control inside is ever replaced,
  // which is the same reason `field` is a getter. Guarded because
  // connectedCallback fires again every time the element is re-parented.
  connectedCallback() {
    if (this.#wired) return;
    this.#wired = true;

    this.addEventListener('keydown', (e) => {
      if (!e.ctrlKey || e.altKey || e.metaKey) return;
      if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
      this.#weight(e, e.key === 'ArrowUp' ? STEP : -STEP);
    });

    // `{ passive: false }` is load-bearing: without it the browser is free to
    // ignore preventDefault() and Ctrl+wheel zooms the page instead. The focus
    // check is the other half — an unfocused field has a stale selectionStart,
    // usually 0, so rolling the wheel over a box you are not editing would
    // silently weight its first word. Unfocused, the gesture stays browser zoom;
    // clicking into the word is how you hand it over. Same shape as the Outputs
    // tab's arrow keys yielding to a focused range input.
    this.addEventListener('wheel', (e) => {
      if (!e.ctrlKey || e.altKey || e.metaKey) return;
      if (document.activeElement !== this.field) return;
      if (!e.deltaY) return;
      this.#weight(e, e.deltaY < 0 ? STEP : -STEP);
    }, { passive: false });
  }

  // One step. `adjust` returns null when there is no word under the caret, and
  // the event is then left alone rather than swallowed — Ctrl+↑ on a blank line
  // should do whatever the browser does with it.
  #weight(e, delta) {
    const f = this.field;
    const next = adjust(f.value, f.selectionStart, f.selectionEnd, delta);
    if (!next) return;
    e.preventDefault();

    // Written through execCommand rather than by assigning `value`, and that is
    // the whole reason this method is four lines instead of one: assigning
    // `value` wipes the field's native undo stack, so one Ctrl+Z after a few
    // nudges would throw the entire prompt away. `insertText` records a single
    // undo entry per adjustment and fires `input` on its own.
    f.focus();
    f.setSelectionRange(0, f.value.length);
    let wrote = false;
    // Deprecated enough to be worth assuming it may throw rather than only
    // return false, and the fallback below is why either can be survived.
    try {
      wrote = document.execCommand('insertText', false, next.text);
    } catch { /* fall through */ }
    if (!wrote) {
      // Keeps the text correct the day execCommand stops working; only the undo
      // history is lost, and `input` then has to be fired by hand.
      f.setRangeText(next.text, 0, f.value.length, 'end');
      f.dispatchEvent(new InputEvent('input', { bubbles: true }));
    }
    // The inner text, never the brackets — which is what lets a held key keep
    // stepping the same group instead of wrapping a new one each time.
    f.setSelectionRange(next.start, next.end);
  }
}

customElements.define('prompt-box', PromptBox);
