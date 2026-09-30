// `node --test 'web/js/*.test.js'`. Not in `web::WEB_FILES`, like its neighbour.
//
// The page's module graph has cycles — the tabs call into each other — and a
// cycle is harmless only while no module *reads* a cyclic import at its top
// level. Break that rule and the browser throws a ReferenceError while
// evaluating the graph, and every tab comes up empty with nothing on screen to
// say why. It has happened: `modelsettings.js` read `settings.js`'s `FIELDS`
// at load, and the model select and the work-item grid both went blank.
//
// So this evaluates the whole graph under Node, with just enough of a DOM
// stubbed for the one custom element to be *declared*. Evaluation must get all
// the way to `boot()` in `app.js` — which is the first thing that needs a real
// `document` — and fail there, and nowhere earlier.
import { test } from 'node:test';
import assert from 'node:assert/strict';

test('every module evaluates, so the only failure is boot() wanting a document', async () => {
  globalThis.HTMLElement ??= class {};
  globalThis.customElements ??= { define() {} };
  await assert.rejects(import('./app.js'), (e) => {
    assert.equal(e.message, 'document is not defined', `evaluating a module failed: ${e.stack}`);
    return true;
  });
});
