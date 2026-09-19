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
import { initHints } from './hints.js';
import { initImage } from './image.js';
import { initWorkItems, markGrid } from './workitems.js';
import { initI2, loadI2Outputs } from './img2img.js';
import { initVideo } from './video.js';
import { initConvert } from './convert.js';
import { stopPlayback } from './player.js';
import './prompt-box.js';

// ---- Tabs -------------------------------------------------------------------

function showTab(name) {
  for (const b of document.querySelectorAll('.tab')) {
    b.classList.toggle('active', b.dataset.tab === name);
  }
  $('tab-image').hidden = name !== 'image';
  $('tab-img2img').hidden = name !== 'img2img';
  $('tab-video').hidden = name !== 'video';
  $('tab-convert').hidden = name !== 'convert';
  // Nothing is playing on a tab nobody is looking at. A 24 fps canvas timer is
  // cheap but it is not free, and it would go on redrawing all night.
  if (name !== 'video') stopPlayback();

  // The two image panes share one `imageId`, so leaving one has to empty it.
  // Otherwise the picture stays on screen beside Refine and Hi-res buttons that
  // now address whatever the other tab produces next — which would work, and
  // would apply the pass to an image other than the one being looked at.
  if (name === 'image' || name === 'img2img') {
    if (S.pane !== name) {
      clearPane(S.pane);
      S.imageId = null;
      S.currentSrc = null;
      S.workId = null;
      markGrid();
      S.pane = name;
    }
    if (name === 'img2img') loadI2Outputs();
  }
}

function initTabs() {
  for (const b of document.querySelectorAll('.tab')) {
    b.addEventListener('click', () => showTab(b.dataset.tab));
  }
}

// The viewer first, because it builds the slider markup that wireHints() then
// has to find; after that the order is only the order the single script used to
// run in, and nothing depends on it.
initViewer();
initHints();
initImage();
initWorkItems();
initI2();
initVideo();
initConvert();
initTabs();
