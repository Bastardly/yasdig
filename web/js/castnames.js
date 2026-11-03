// Which saved cast members a prompt names, pure (`castnames.test.js`).
//
// A name counts as a whole word, written as the entry spells it: "Elias
// lights a cigar" names Elias, "Eliason" doesn't. Case is kept on purpose —
// a cast named "Rain" or "Hope" would otherwise be found in every rainy,
// hopeful prompt. It is only ever an offer ("Elias is in your cast — Add"),
// never a pick made for the person.

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

// Entries `{id, name}` whose name is in `prompt` and whose id isn't in
// `picked`, in the order the prompt first names them.
export function namedIn(prompt, entries, picked = []) {
  const found = [];
  for (const e of entries) {
    const name = (e.name ?? '').trim();
    if (!name || picked.includes(e.id)) continue;
    // Letters and digits of any script are word characters here, so "Zoë"
    // and "Élise" end where a reader thinks they do.
    const re = new RegExp(`(?<![\\p{L}\\p{N}])${escapeRe(name)}(?![\\p{L}\\p{N}])`, 'u');
    const m = re.exec(prompt);
    if (m) found.push({ e, at: m.index });
  }
  return found.sort((a, b) => a.at - b.at).map((f) => f.e);
}
