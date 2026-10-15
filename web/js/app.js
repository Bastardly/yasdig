// The page's entry point: it defines the one custom element, owns the only
// genuinely cross-tab function, and runs every module's init in order.
//
// A module's top level declares things and nothing else. All the wiring and all
// the first fetches happen in the `init*` functions called from `boot()` below,
// which is what makes the import graph safe to have cycles in — the video tab
// and its frame player import each other, and neither reads the other at
// evaluation time.
import { $ } from './dom.js';
import { S, clearPane } from './state.js';
import { initViewer } from './viewer.js';
import { initRedraw } from './redraw.js';
import { initRefs } from './refs.js';
import { initHints } from './hints.js';
import { initImage, layoutImageTab, restoreImage } from './image.js';
import { initJobs, jobCount } from './jobs.js';
import { initBusy } from './busy.js';
import { initWorkItems, markGrid } from './workitems.js';
import { initOutputs, loadOutputs } from './outputs.js';
import { initVideo, layoutVideoTab } from './video.js';
import { initConvert } from './convert.js';
import { initSettings, layoutSettingsTab } from './settings.js';
import { initHelp, openHelp } from './help.js';
import { initGetModels } from './getmodels.js';
import { initModelSettings } from './modelsettings.js';
import { stopPlayback } from './player.js';
import './prompt-box.js';

// ---- Tabs -------------------------------------------------------------------

function showTab(name) {
  for (const b of document.querySelectorAll('.tab')) {
    b.classList.toggle('active', b.dataset.tab === name);
  }
  $('tab-image').hidden = name !== 'image';
  $('tab-outputs').hidden = name !== 'outputs';
  $('tab-video').hidden = name !== 'video';
  $('tab-convert').hidden = name !== 'convert';
  $('tab-getmodels').hidden = name !== 'getmodels';
  $('tab-help').hidden = name !== 'help';
  $('tab-settings').hidden = name !== 'settings';
  // A hidden workspace has no size: its panels are placed once it shows.
  if (name === 'image') layoutImageTab();
  if (name === 'video') layoutVideoTab();
  if (name === 'settings') layoutSettingsTab();
  // Nothing is playing on a tab nobody is looking at. A 24 fps canvas timer is
  // cheap but it is not free, and it would go on redrawing all night.
  if (name !== 'video') stopPlayback();

  // The Refine & adjust dialog's saves, in its rail: the Image tab keeps a
  // picture with Save to output, the Outputs tab saves over the file or beside
  // it.
  $('lbsave').hidden = name !== 'outputs';
  $('lbsavechanges').hidden = name !== 'outputs';
  $('saveout').hidden = name !== 'image';

  // The two image panes share one `imageId`, so leaving one has to empty it.
  // Otherwise the picture stays on screen beside Refine and Hi-res buttons that
  // now address whatever the other tab produces next — which would work, and
  // would apply the pass to an image other than the one being looked at.
  if (name === 'image' || name === 'outputs') {
    if (S.pane !== name) {
      clearPane(S.pane);
      S.imageId = null;
      S.currentSrc = null;
      S.workId = null;
      markGrid();
      S.pane = name;
      // The Image tab's last result comes back with the tab: it is still
      // retained on the server (the Outputs viewer has a slot of its own), and
      // a run may have made a newer one while you were away.
      if (name === 'image') restoreImage();
    }
    // Re-read on entry: the Image tab's Save writes into this directory, so the
    // grid is stale the moment you come back from having used it.
    if (name === 'outputs') loadOutputs();
  }
}

function initTabs() {
  for (const b of document.querySelectorAll('.tab')) {
    b.addEventListener('click', () => showTab(b.dataset.tab));
  }
  $('closeserver').addEventListener('click', closeServer);
  $('licenceslink').addEventListener('click', (e) => {
    e.preventDefault();
    openHelp('about');
  });
}

// ---- Close server -----------------------------------------------------------
//
// `POST /shutdown` kills a render in flight, clears what the *saved* settings
// say to, and exits (`handle_shutdown`). The confirm quotes that saved file
// rather than the Settings form, because a box ticked and not saved is not what
// the server will read — and a confirm that listed it would be promising a
// delete that never happens.
async function closeServer() {
  let s = {};
  try { s = await (await fetch('/settings')).json(); } catch { /* the plain question below */ }
  const doomed = [
    s.clear_workitems_on_close && 'work items (Current, not Favorites)',
    s.clear_outputs_on_close && 'outputs in work/output/ (not Favorites)',
    // Only while the Video tab is on: a hidden feature isn't mentioned.
    s.nightly_video && s.clear_videos_on_close && 'video clips',
  ].filter(Boolean);
  const queued = jobCount();
  const msg = 'Close the yasdig server?\n\n'
    + 'If an image or video is being made, it is stopped. '
    + (queued ? `The ${queued} job${queued > 1 ? 's' : ''} in the queue ${queued > 1 ? 'are' : 'is'} dropped. ` : '')
    + (doomed.length
      ? `These are deleted, as your saved settings ask:\n• ${doomed.join('\n• ')}`
      : 'Nothing is deleted.');
  if (!confirm(msg)) return;
  try {
    // `clear` said out loud: the server deletes nothing unless asked to.
    const resp = await fetch('/shutdown', { method: 'POST', body: JSON.stringify({ clear: true }) });
    await resp.json();
  } catch {
    // The server exits right after answering, so a connection that drops here
    // has done what was asked. Either way it is gone.
  }
  // Replaced first: that is also what stands Settings' "unsaved changes"
  // prompt down (it checks the form is still there), since with no server
  // there is nothing left to save to.
  document.querySelector('.app').innerHTML =
    '<div class="card stopped"><h1>yasdig has stopped</h1>'
    + '<p class="sub">You can close this tab. Start yasdig again to come back.</p></div>';
  // Browsers only let a script close a tab a script opened; this one was
  // opened by the OS. So try; when it is ignored, the message above stays.
  window.close();
}

// The viewer first, because it builds the slider markup that wireHints() then
// has to find; after that the order is only the order the single script used to
// run in, and nothing depends on it.
initViewer();
initHints();
initBusy();
// Before initImage(): its model list calls syncRefs as soon as it loads.
initRefs();
initImage();
// After initViewer(): it hooks into the dialog viewer.js owns, and its mask's
// pointerdown must come after the pane's pan listener.
initRedraw();
initWorkItems();
initOutputs();
initVideo();
initConvert();
initGetModels();
initHelp();
// After initImage(), because it applies the stored hi-res scale through
// `setUpFactor`, which addresses a select initImage() fills the options of.
// That puts it after initHints(), so it wires its own generated labels — the
// one module that does, and the reason `wireHints` takes a root.
// Its form is built synchronously here; only the values wait on the fetch.
initSettings();
// After initImage(), whose model list starts the upscaler fetch this waits on
// for its own select, and after initHints(), since it wires its own labels.
initModelSettings();
// Last: its first poll hands any image a run already made to the hooks the
// modules above registered, so they have to be registered first.
initJobs();
initTabs();
