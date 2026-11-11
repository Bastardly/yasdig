---
paths:
  - "src/jobs.rs"
  - "src/shutdown.rs"
  - "web/js/jobs.js"
  - "web/js/busy.js"
  - "web/js/alive.js"
---

# The job queue and Close server

## The job queue

`src/jobs.rs` (`Queue<T>`, generic for GPU-free tests), `Work`/`diffuse` in `main.rs`,
`web/js/jobs.js`. Every diffusion-lane job (run, Refine/Hi-res press, clip, clip
enlarge, conversion) is an entry, stoppable in the Jobs dialog. The display lane
(`pipeline::Display`: open, adjust, open-output, save) is separate.

**A run is handed out one image at a time** (`Queue::next`, `run_image`, `image_done`):

- **A button press runs at the next image boundary** (`priority`: after the running
  entry and any waiting priority entries).
- **A loop gives way**: after each image, if anything waits, it goes to the back.
- **Stop**: a waiting entry is removed (request answered, pin released); a running run
  is marked `stopping` and ends at the image boundary; other sd-cli work is killed
  (`monitor.cancel`); FSR/`sr` work answers 400 (it will finish).
- **Cancel generation** (`POST /jobs/skip`): `Queue::skip` marks *before* the kill;
  `diffuse` reads `Queue::skipped` and calls `image_done`, not `fail_run`. The skip
  counts toward `done`. The mark is taken back inside `skip` when no child was found. Refine/Hi-res presses use
  `/jobs/stop`.

Break silently:

- **A worker panic fails the job, never the thread**: `catch_unwind` + `Queue::abort`.
  `Fsr::process` checks sizes (`check_fits`, `max_side` = adapter's texture limit) and
  runs in a wgpu error scope (the uncaptured handler panics). `Settings::max_image_side`
  (default 8192, page-level) is checked by `Engine::check_side` before any work.
- **A run answers `{job}` at once; images come via `/jobs/latest`** (`set_latest`,
  `latest_seq`). Refine, Hi-res, Video, Convert, Enlarge answer on their own request
  (`enqueue_wait`) and also publish; `takeResult` skips one already shown.
- **`Queue::changed` is the page's refresh signal**: every method that alters the
  snapshot takes `Queue::change()` (the lock, plus a bump), `next` bumps on handing out;
  `every_change_the_page_could_show_moves_the_counter`. The page's `queueChanged`
  (`jobs.js`, an observable with `at`) notifies on a move; a panel showing what a job
  writes subscribes and refetches **without gating on its own idea of busy** (the
  Assistant's `refresh`). The poll's fetches time out (`POLL_TIMEOUT_MS`) and a
  throwing hook is logged, never allowed to stop the poll — `aprog` once did.
- **Everything a run needs is captured at queue time — except the loop.** There is
  one (`Queue::push_loop`); a loop request updates it (kind, label, work; id and
  `done` kept), and the page re-sends the panel on change (`syncLoop` from
  `syncSummaries`: debounced, deduplicated, skipped on a blank prompt, `update_only`
  so a stale edit never restarts an ended loop).
- **Liveness is `alive.js`'s alone** (`GET /health` every 2 s, a banner after two
  misses, a top-layer `popover` so it shows over a modal dialog): the jobs poll and
  every other fetch stay quiet on failure. Close server calls `stopAlive`/`stopJobs`
  before its request.
- **A run's failure has no request**: `fail_run` ends it with a `Notice`; the page's
  first poll adopts existing notices.
- **`/generate` validates only the count**; everything else surfaces as a notice.

The display lane shares FSR as `Arc<Mutex<Fsr>>`; the diffusion lane locks per FSR
step, never across sd-cli (`enlarge` doesn't lock around ESRGAN). `Retained::update`
writes back only over the entry it read; an `/adjust` that loses is
`pipeline::Superseded` → 409, which the page ignores (never null `S.imageId` on it).
**Stop/skip kill under the queue lock** (`Queue::stop`/`skip` take the kill): checking
then killing could hit the next image's sd-cli. **`Work::pinned()` is the one list of
pinning kinds** — no `_` arm.

## Close server

`POST /shutdown` (`handle_shutdown`, `src/shutdown.rs`), on the connection thread, in
order: **kill the child**, clear, answer, `process::exit(0)`. The clear reads the
*saved* `user/settings.toml` (the confirm quotes the same file); deletes only through
`Store::delete`/`delete_output`/`VideoStore::delete`, never favourites, never
`remove_dir_all`. Always empties `work/current/` (`pipeline::clear_scratch`); the
confirm counts dropped queued jobs. The `clear_*_on_close` fields are page-level.
**Page-level keys are marked `page: true` on their `FIELDS` row** so the *Per model*
card omits them (`fields_mark_exactly_the_page_only_keys`). The page then shows a
"stopped" note (`window.close()` is ignored); Settings' `beforeunload` stands down.
