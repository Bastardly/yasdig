// The prompt input, as one element used six times: a prompt and a negative on
// each of the three tabs that ask for one. It is where the features go; today it
// is only the wrapper that makes "the prompt" one thing on the page rather than
// six controls that happen to look alike.
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
class PromptBox extends HTMLElement {
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
}

customElements.define('prompt-box', PromptBox);
