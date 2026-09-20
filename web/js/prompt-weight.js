// The string half of Ctrl+↑ / Ctrl+↓ over a prompt: given the field's text and
// where the selection is, work out the same edit ComfyUI and Forge make. No DOM
// and no imports, same as dom.js — everything here is a pure function of a
// string and two offsets, which is what makes `prompt-weight.test.js` able to
// cover the dozen boundary cases without a browser.
//
// The syntax is `(word:1.2)`: round parens, a dot decimal. That is what the
// binary we shell out to parses — `parse_prompt_attention`
// (stable-diffusion.cpp src/core/util.cpp:816), a port of A1111's — and three
// facts about it decide the whole design below:
//
//   • Nesting *multiplies*, it does not replace: `(((house:1.3)))` conditions at
//     1.573 (util.cpp:806-815). So when the caret is already inside a group we
//     retune that group's number rather than wrapping a second one round it,
//     which would silently push a 1.2 to 1.32.
//   • `(x)` is ×1.1 and `[x]` is ×1/1.1 (util.cpp:821-822). A hand-typed `[cat]`
//     therefore has to be *recognised*; nesting it into `([cat]:1.1)` would
//     cancel out to exactly 1.0 and look like the key did nothing.
//   • `\(` is the one working escape (util.cpp:892), so a literal paren the user
//     escaped must not be read as a group. We never emit an escape ourselves —
//     `\)` is a parser bug upstream and leaks a backslash into the tokens.

// One step per keypress or wheel notch, and the grid every weight is snapped to.
// A tenth is what both ComfyUI and Forge use, and it is coarse enough that the
// printed number is always one decimal.
export const STEP = 0.1;

// Where a word ends. Whitespace plus the punctuation a prompt actually uses to
// separate ideas — commas above all — and every bracket, so expanding a word
// never walks out through one.
const DELIM = /[\s,.:;!?()[\]{}|/\\"]/;

// `(word:1.2)` — the tail we rewrite. Anchored at the end of a group's inner
// text, and deliberately permissive about sign and shape (`-.5` parses), because
// this text may have been typed by hand. It reads `(a ratio of 16:9)` as the
// words `a ratio of 16` at weight 9, which looks like a misparse and is not —
// that is exactly what sd.cpp does with the same characters, and disagreeing
// with the parser would be the real bug.
const WEIGHT_TAIL = /:\s*([-+]?(?:\d+\.?\d*|\.\d+))\s*$/;

// What a bare bracket pair means, per util.cpp:821-822. The square form is
// 1/1.1 = 0.909…, snapped to the step grid: a tenth of one step, in a number the
// user is in the middle of dragging, and the alternative is a prompt that reads
// `(cat:0.8090909090909091)` after one keypress.
const BARE = { '(': 1.1, '[': 0.9 };
const CLOSER = { '(': ')', '[': ']' };

// A backslash escapes the character after it, but only an *unescaped* backslash
// does — `\\(` is a literal backslash followed by a real opening paren. Counting
// the run backwards is the only way to tell those apart.
function escaped(text, i) {
  let n = 0;
  while (i - n - 1 >= 0 && text[i - n - 1] === '\\') n++;
  return n % 2 === 1;
}

// Expand a caret to the word it sits in. Returns null when it sits on a
// delimiter instead — there is no word to weight, and the caller must then leave
// the keystroke alone rather than swallowing it.
function wordAt(text, pos) {
  let a = pos;
  let b = pos;
  while (a > 0 && !DELIM.test(text[a - 1])) a--;
  while (b < text.length && !DELIM.test(text[b])) b++;
  return b > a ? { start: a, end: b } : null;
}

// The innermost `(…)` or `[…]` that contains [start, end), or null. One left-to-
// right pass with a stack, so nesting costs nothing extra and an unbalanced
// bracket is simply never closed and never matched — which is also what keeps an
// unterminated `(` from making this loop forever.
function enclosing(text, start, end) {
  const stack = [];
  let best = null;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (escaped(text, i)) continue;
    if (c === '(' || c === '[') {
      stack.push(i);
    } else if (c === ')' || c === ']') {
      const open = stack.pop();
      if (open === undefined || CLOSER[text[open]] !== c) continue;
      // Innermost wins, and a later pass can only ever find a wider one at the
      // same depth, so the first containing pair is the answer.
      if (open < start && end <= i && best === null) best = { open, close: i };
    }
  }
  return best;
}

// A mouse selection usually takes the brackets with it: dragging across
// `(cat:1.2)` selects all nine characters, not the three inside. Recognise a
// range that *is* a balanced group so that case retunes the number too, rather
// than producing `((cat:1.2):1.1)`.
function spansGroup(text, start, end) {
  const open = text[start];
  if (!CLOSER[open] || escaped(text, start)) return false;
  let depth = 0;
  for (let i = start; i < end; i++) {
    if (escaped(text, i)) continue;
    if (CLOSER[text[i]]) {           // an opener: '(' or '['
      depth++;
    } else if (text[i] === ')' || text[i] === ']') {
      depth--;
      // The first time we come back to zero must be the last character, or the
      // range holds a closed group plus something else.
      if (depth === 0) return i === end - 1 && text[i] === CLOSER[open];
    }
  }
  return false;
}

// Snap to the step grid. Not cosmetic: 1.1 + 0.1 is 1.2000000000000002 in
// IEEE754, and that is what would land in the prompt. `|| 0` kills the negative
// zero that Math.round(-0.04 * 10) produces, which prints as `-0.0`.
const snap = (w) => (Math.round(w * 10) / 10) || 0;

/**
 * The edit one Ctrl+↑ / Ctrl+↓ (or one wheel notch) makes.
 *
 * @param {string} text   the field's current contents
 * @param {number} start  selectionStart
 * @param {number} end    selectionEnd
 * @param {number} delta  +STEP or -STEP
 * @returns {{text: string, start: number, end: number}|null}
 *   the new contents and where to put the selection, or null when there is
 *   nothing under the caret to weight — in which case the caller must *not*
 *   preventDefault, so Ctrl+↑ keeps whatever the browser does with it.
 */
export function adjust(text, start, end, delta) {
  let target = start === end ? wordAt(text, start) : { start, end };
  if (!target) return null;

  const group = spansGroup(text, target.start, target.end)
    ? { open: target.start, close: target.end - 1 }
    : enclosing(text, target.start, target.end);

  let inner = text.slice(target.start, target.end);
  let weight = 1;

  if (group) {
    const body = text.slice(group.open + 1, group.close);
    const m = body.match(WEIGHT_TAIL);
    const bare = m ? body.slice(0, m.index) : body;
    // Retune the group only when the target *is* the group — arrived at either
    // from inside it (the first test) or by having selected it whole (the
    // second, where `inner` still carries the brackets and so can never match).
    // Anything else nests, which is the right answer and not a fallback: a
    // caret on `cat` inside `(a red cat:1.2)` gives `(a red (cat:1.1):1.2)`,
    // and pointing at one word of three is a request to weight that word.
    if (bare === inner || group.open === target.start) {
      inner = bare;
      weight = m ? parseFloat(m[1]) : BARE[text[group.open]];
      target = { start: group.open, end: group.close + 1 };
    }
  }

  const next = snap(weight + delta);
  // Exactly 1.0 is no weighting at all, so the wrapper goes and the bare words
  // come back — the round trip has to end where it started or the syntax
  // accumulates in the box forever.
  const out = next === 1 ? inner : `(${inner}:${next.toFixed(1)})`;
  // The selection comes back as the inner text, never the brackets. That is what
  // makes a held key idempotent: every step leaves the selection equal to the
  // group's inner text again, so the branch above keeps being the one taken.
  const offset = target.start + (next === 1 ? 0 : 1);
  return {
    text: text.slice(0, target.start) + out + text.slice(target.end),
    start: offset,
    end: offset + inner.length,
  };
}
