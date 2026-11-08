// Enhance: a prompt box's words rewritten in full by the agent's model
// (`POST /enhance`, `agent::enhance`), for the model the box is for — the
// Image tab's prompt and each Video shot. The answer replaces the text with
// `insertText`, so one Ctrl+Z brings the person's own words back. While it
// runs the button reads Stop (`stopKind('enhance')`; the server kills it).
import { post } from './dom.js';
import { stopKind } from './jobs.js';

// Replace a field's whole text as one undo step, firing `input` either way.
export function replaceText(field, text) {
  field.focus();
  field.setSelectionRange(0, field.value.length);
  let wrote = false;
  try {
    wrote = document.execCommand('insertText', false, text);
  } catch { /* fall through */ }
  if (!wrote) {
    field.setRangeText(text, 0, field.value.length, 'end');
    field.dispatchEvent(new InputEvent('input', { bubbles: true }));
  }
}

// A click on an Enhance button: start, or stop the one it started.
//   field     the text box written to
//   body      the request, or null to do nothing
//   onError   (message) — '' clears
//   onDone    (answer) after the text is written: H3's `sound`, say
// True when the text was written, so a caller can go on to the next box.
export async function enhance(btn, field, body, { onError, onDone } = {}) {
  if (btn.dataset.busy) { stopKind('enhance'); return false; }
  if (!body) return false;
  if (!body.text?.trim()) { onError?.('Write a few words first: Enhance fills in what they leave open.'); return false; }
  const label = btn.textContent;
  btn.dataset.busy = '1';
  btn.textContent = 'Stop';
  btn.title = 'Stop enhancing';
  onError?.('');
  try {
    const r = await post('/enhance', body);
    // A box redrawn meanwhile (another clip opened) isn't the one asked for.
    if (field.isConnected && r.prompt) {
      replaceText(field, r.prompt);
      onDone?.(r);
      return true;
    }
    return false;
  } catch (e) {
    onError?.(e.message);
    return false;
  } finally {
    delete btn.dataset.busy;
    btn.textContent = label;
    btn.title = btn.dataset.title ?? '';
  }
}
