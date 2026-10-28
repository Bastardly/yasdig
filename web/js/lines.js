// ---- Spoken lines -------------------------------------------------------------
//
// What a shot says, as the Story panel and the Video tab's Shots panel both
// edit it: one line each, `Who: words`, with `(voiceover)` and a language
// other than English in brackets after the name. The server writes them into
// H3's prompt layout (`h3prompt.rs`). Pure, for `lines.test.js`.

// A shot's lines as the panels edit them.
export function linesText(lines = []) {
  return lines.map((l) => {
    const tags = [l.voiceover ? 'voiceover' : '', l.language && l.language !== 'English' ? l.language : ''].filter(Boolean);
    return `${l.who}${tags.length ? ` (${tags.join(', ')})` : ''}: ${l.words}`;
  }).join('\n');
}

// The panel's text back into lines. A line with no `Who:` keeps an empty
// speaker, which the server refuses with the reason.
export function parseLines(text) {
  return text.split('\n').map((row) => row.trim()).filter(Boolean).map((row) => {
    const m = /^([^:()]+?)\s*(?:\(([^)]*)\))?\s*:\s*(.*)$/.exec(row);
    if (!m) return { who: '', words: row, language: '', voiceover: false };
    const tags = (m[2] ?? '').split(',').map((t) => t.trim()).filter(Boolean);
    const voiceover = tags.some((t) => t.toLowerCase() === 'voiceover');
    const language = tags.find((t) => t.toLowerCase() !== 'voiceover') ?? '';
    return { who: m[1].trim(), words: m[3].trim(), language, voiceover };
  });
}
