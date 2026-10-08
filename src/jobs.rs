//! The job queue: every piece of diffusion-lane work, in the order it will run.
//!
//! Before this, a batch or the loop was a loop *in the page* — `/generate`,
//! `/hires`, `/refine`, `/save-output` per image — and the worker took one
//! request at a time. That was a deliberate choice with two arguments behind it,
//! and both are answered here rather than dropped:
//!
//! * **"One job that held the worker for a whole run would starve everything
//!   else."** A run is not one job to the worker. [`Queue::next`] hands out one
//!   *image* of it at a time, and after each one the front of the queue is read
//!   again — so a Refine pressed mid-batch runs at the next image boundary, and
//!   the display passes (`/adjust`, opening an output) are on a lane of their
//!   own and never wait on this queue at all.
//! * **"Cancelling would need a second channel into the loop."** That channel is
//!   this struct: `/jobs/stop` writes `stopping` here and the worker reads it at
//!   the image boundary, which is exactly where the page's Stop used to act.
//!
//! What the move buys is that the run is the server's: a reload, a tab switch or
//! a second job queued behind it loses nothing, and the Jobs dialog can show and
//! stop what is waiting.
//!
//! Generic over the work it carries so the scheduling rules can be tested
//! without a GPU or a `GenRequest`; `main.rs` instantiates it with its `Work`.
//!
//! The queue is memory only. A restart drops it, and *Close server* says so.

use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// What a job is, as the Jobs dialog names it. Only the two image runs can be
/// more than one image long, which is what [`Kind::is_run`] is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Generate,
    Img2img,
    Refine,
    Hires,
    /// Redraw a painted region of the image in hand. One pass, so a Stop
    /// kills it like a Refine.
    Redraw,
    /// Enlarge the image in hand with FSR or an upscaler model, no diffusion.
    /// One pass, like a Refine.
    Upscale,
    Video,
    Enlarge,
    Convert,
}

impl Kind {
    /// A run is scheduled image by image and stopped *between* images; every
    /// other kind is one indivisible job that a Stop can only kill.
    pub fn is_run(self) -> bool {
        matches!(self, Kind::Generate | Kind::Img2img)
    }
}

struct Entry<T> {
    id: u64,
    kind: Kind,
    label: String,
    done: u32,
    /// `None` is "Keep going until stopped".
    total: Option<u32>,
    stopping: bool,
    /// The image in flight was cancelled on purpose (*Cancel generation*):
    /// its failure is the next image's cue, not the end of the run.
    skipping: bool,
    /// Pushed by a button press on the image in hand (Refine, Hi-res) rather
    /// than queued as new work: it goes ahead of everything that isn't running.
    priority: bool,
    work: T,
    /// Where a waiting HTTP request gets its answer. `None` for a run, whose
    /// route answered `{job}` the moment it was queued.
    reply: Option<Sender<Result<String>>>,
}

/// One unit for the worker: the whole job, or one image of a run.
pub struct Ticket<T> {
    pub id: u64,
    /// Images of this run already finished — the seed walk reads it.
    pub done: u32,
    pub work: T,
}

/// What [`Queue::stop`] did, so the caller can do the half it owns.
pub enum Stopped<T> {
    /// It was waiting and is gone; its work comes back so anything it held
    /// (a pinned image) can be let go.
    Removed(T),
    /// A run, now finishing the image in flight and then ending.
    Stopping,
    /// Running and indivisible: the caller kills the child.
    Cancel,
    Unknown,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Notice {
    pub seq: u64,
    pub message: String,
}

#[derive(serde::Serialize)]
pub struct JobView {
    pub id: u64,
    pub kind: Kind,
    pub label: String,
    pub done: u32,
    pub total: Option<u32>,
    /// `running`, `stopping` or `queued`.
    pub state: &'static str,
    /// The image in flight is being cancelled (*Cancel generation*). Beside
    /// `state` rather than one of its values, because a run can be stopping
    /// and skipping at once.
    pub skipping: bool,
}

#[derive(serde::Serialize)]
pub struct Snapshot {
    pub jobs: Vec<JobView>,
    /// Bumped each time [`Queue::set_latest`] publishes an image, so the page
    /// knows to fetch it without being sent a megabyte of base64 per poll.
    pub latest_seq: u64,
    pub notices: Vec<Notice>,
}

/// Failures kept for the page to pick up. A run that fails has no request
/// waiting on it, so this is the only way its message reaches anybody; a few
/// is plenty, since the page reads them within a second.
const MAX_NOTICES: usize = 8;

struct State<T> {
    entries: VecDeque<Entry<T>>,
    /// The id of the entry the worker holds. Always the front entry while set.
    running: Option<u64>,
    next_id: u64,
    seq: u64,
    latest: Option<(u64, Arc<String>)>,
    notices: VecDeque<Notice>,
}

pub struct Queue<T> {
    inner: Arc<(Mutex<State<T>>, Condvar)>,
}

impl<T> Clone for Queue<T> {
    fn clone(&self) -> Self {
        Queue {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Clone> Default for Queue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> Queue<T> {
    pub fn new() -> Self {
        Queue {
            inner: Arc::new((
                Mutex::new(State {
                    entries: VecDeque::new(),
                    running: None,
                    next_id: 1,
                    seq: 0,
                    latest: None,
                    notices: VecDeque::new(),
                }),
                Condvar::new(),
            )),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        // A panic while holding this lock is a bug in this file, and the data
        // is still a valid queue — keep serving rather than poisoning every
        // connection thread with it.
        self.inner.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue a job and return its id. `total` is the number of images for a
    /// run (`None` loops) and is ignored for any other kind.
    ///
    /// `priority` puts it after the running entry and after any priority entry
    /// already waiting, so two button presses still run in the order pressed.
    pub fn push(
        &self,
        kind: Kind,
        label: String,
        total: Option<u32>,
        work: T,
        reply: Option<Sender<Result<String>>>,
        priority: bool,
    ) -> u64 {
        let mut s = self.lock();
        let id = s.next_id;
        s.next_id += 1;
        let total = if kind.is_run() { total.map(|n| n.max(1)) } else { Some(1) };
        let entry = Entry {
            id,
            kind,
            label,
            done: 0,
            total,
            stopping: false,
            skipping: false,
            priority,
            work,
            reply,
        };
        if priority {
            let start = usize::from(s.running.is_some());
            let at = start
                + s.entries
                    .iter()
                    .skip(start)
                    .take_while(|e| e.priority)
                    .count();
            s.entries.insert(at, entry);
        } else {
            s.entries.push_back(entry);
        }
        drop(s);
        self.inner.1.notify_all();
        id
    }

    /// Block until there is work, then hand out the front entry: the whole of
    /// it, or for a run the next image.
    pub fn next(&self) -> Ticket<T> {
        let mut s = self.lock();
        loop {
            if let Some(front) = s.entries.front() {
                let t = Ticket {
                    id: front.id,
                    done: front.done,
                    work: front.work.clone(),
                };
                s.running = Some(t.id);
                return t;
            }
            s = self.inner.1.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// One image of a run is finished — its hi-res, refine and save included.
    /// Returns whether the run goes on.
    ///
    /// Going on does not mean going *first*. The entry steps behind any
    /// priority entries that arrived meanwhile, so a Refine pressed during a
    /// batch runs at this boundary; and a loop steps behind *everything*, since
    /// it would otherwise hold the queue for ever and every job after it would
    /// be a job that never runs.
    pub fn image_done(&self, id: u64) -> bool {
        let mut s = self.lock();
        s.running = None;
        let Some(i) = s.entries.iter().position(|e| e.id == id) else {
            return false;
        };
        let mut e = s.entries.remove(i).expect("position came from this deque");
        e.done += 1;
        e.skipping = false;
        let finished = e.stopping || e.total.is_some_and(|n| e.done >= n);
        if finished {
            return false;
        }
        if e.total.is_none() && !s.entries.is_empty() {
            s.entries.push_back(e);
        } else {
            let at = s.entries.iter().take_while(|x| x.priority).count();
            s.entries.insert(at, e);
        }
        true
    }

    /// *Cancel generation*: mark the image a running run has in flight as
    /// cancelled on purpose, so the caller can kill its child and the worker
    /// counts the failure that follows as the image being done rather than as
    /// the run failing. Returns whether `id` is a run the worker holds; anything
    /// else has no "next image" to jump to, and is the Stop route's to kill.
    pub fn skip(&self, id: u64) -> bool {
        let mut s = self.lock();
        if s.running != Some(id) {
            return false;
        }
        match s.entries.iter_mut().find(|e| e.id == id) {
            Some(e) if e.kind.is_run() => {
                e.skipping = true;
                true
            }
            _ => false,
        }
    }

    /// Take a [`Queue::skip`] back: there was no child to kill, so nothing will
    /// fail, and a mark left standing would swallow a real failure later on.
    pub fn unskip(&self, id: u64) {
        if let Some(e) = self.lock().entries.iter_mut().find(|e| e.id == id) {
            e.skipping = false;
        }
    }

    /// Whether the image that just failed was cancelled on purpose.
    pub fn skipped(&self, id: u64) -> bool {
        self.lock().entries.iter().any(|e| e.id == id && e.skipping)
    }

    /// An indivisible job is over: answer whoever is waiting and drop it.
    pub fn finish(&self, id: u64, result: Result<String>) {
        let mut s = self.lock();
        s.running = None;
        if let Some(i) = s.entries.iter().position(|e| e.id == id) {
            let e = s.entries.remove(i).expect("position came from this deque");
            if let Some(reply) = e.reply {
                let _ = reply.send(result);
            }
        }
    }

    /// A run failed partway. Whatever killed this image would kill the next
    /// one too — VRAM, a missing checkpoint, a pass the model hasn't got — so
    /// the run ends here rather than repeating it, and the message is kept for
    /// the page, since no request is waiting on a run.
    pub fn fail_run(&self, id: u64, message: String) {
        let mut s = self.lock();
        s.running = None;
        if let Some(i) = s.entries.iter().position(|e| e.id == id) {
            s.entries.remove(i);
        }
        s.seq += 1;
        let seq = s.seq;
        s.notices.push_back(Notice { seq, message });
        while s.notices.len() > MAX_NOTICES {
            s.notices.pop_front();
        }
    }

    /// The Jobs dialog's Stop and Remove, which are one button: what it does
    /// depends on whether the worker has the job yet.
    pub fn stop(&self, id: u64) -> Stopped<T> {
        let mut s = self.lock();
        let Some(i) = s.entries.iter().position(|e| e.id == id) else {
            return Stopped::Unknown;
        };
        if s.running == Some(id) {
            let e = &mut s.entries[i];
            if e.kind.is_run() {
                e.stopping = true;
                return Stopped::Stopping;
            }
            return Stopped::Cancel;
        }
        let e = s.entries.remove(i).expect("position came from this deque");
        if let Some(reply) = e.reply {
            let _ = reply.send(Err(anyhow!("removed from the job queue")));
        }
        Stopped::Removed(e.work)
    }

    /// Publish an image result for the page to pick up.
    pub fn set_latest(&self, json: String) {
        let mut s = self.lock();
        s.seq += 1;
        let seq = s.seq;
        s.latest = Some((seq, Arc::new(json)));
    }

    pub fn latest(&self) -> Option<(u64, Arc<String>)> {
        self.lock().latest.clone()
    }

    /// What `GET /jobs` serves: every entry in run order, the running one first.
    pub fn snapshot(&self) -> Snapshot {
        let s = self.lock();
        let jobs = s
            .entries
            .iter()
            .map(|e| JobView {
                id: e.id,
                kind: e.kind,
                label: e.label.clone(),
                done: e.done,
                total: e.total,
                state: if s.running != Some(e.id) {
                    "queued"
                } else if e.stopping {
                    "stopping"
                } else {
                    "running"
                },
                skipping: e.skipping,
            })
            .collect();
        Snapshot {
            jobs,
            latest_seq: s.latest.as_ref().map_or(0, |(seq, _)| *seq),
            notices: s.notices.iter().cloned().collect(),
        }
    }

    /// How many jobs are queued or running.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }
}

/// The seed for image `done` of a run: a pinned seed walks by one per image,
/// because a batch at a fixed seed is otherwise N copies of one picture, and
/// image 0 is still exactly the seed in the box. -1 stays -1 and the engine
/// draws a fresh one each time. Moved here from the page's `genPayload(index)`.
pub fn walk_seed(seed: i64, done: u32) -> i64 {
    if seed < 0 {
        -1
    } else {
        seed.saturating_add(done as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn ids(q: &Queue<&'static str>) -> Vec<&'static str> {
        q.lock().entries.iter().map(|e| e.work).collect()
    }

    #[test]
    fn a_batch_runs_its_count_and_ends() {
        let q = Queue::new();
        let id = q.push(Kind::Generate, "m".into(), Some(3), "batch", None, false);
        for n in 0..3 {
            let t = q.next();
            assert_eq!((t.id, t.done), (id, n));
            assert_eq!(q.image_done(id), n < 2);
        }
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn a_loop_yields_to_a_job_queued_behind_it() {
        let q = Queue::new();
        let lp = q.push(Kind::Generate, "m".into(), None, "loop", None, false);
        assert_eq!(q.next().id, lp);
        q.push(Kind::Generate, "m".into(), Some(1), "after", None, false);
        assert!(q.image_done(lp));
        assert_eq!(ids(&q), ["after", "loop"]);
        // Alone, it simply carries on.
        let t = q.next();
        assert_eq!(t.work, "after");
        assert!(!q.image_done(t.id));
        assert_eq!(q.next().id, lp);
        assert!(q.image_done(lp));
        assert_eq!(ids(&q), ["loop"]);
    }

    #[test]
    fn a_button_press_runs_at_the_next_image_boundary() {
        let q = Queue::new();
        let batch = q.push(Kind::Generate, "m".into(), Some(4), "batch", None, false);
        q.push(Kind::Video, "v".into(), None, "video", None, false);
        assert_eq!(q.next().id, batch);
        // Pressed while the batch's first image is in flight: not ahead of the
        // running entry, but ahead of everything else, in the order pressed.
        q.push(Kind::Refine, "m".into(), None, "refine", None, true);
        q.push(Kind::Hires, "m".into(), None, "hires", None, true);
        assert_eq!(ids(&q), ["batch", "refine", "hires", "video"]);
        assert!(q.image_done(batch));
        assert_eq!(ids(&q), ["refine", "hires", "batch", "video"]);
    }

    #[test]
    fn removing_a_waiting_job_answers_its_request() {
        let q = Queue::new();
        q.push(Kind::Generate, "m".into(), Some(1), "first", None, false);
        let (tx, rx) = mpsc::channel();
        let video = q.push(Kind::Video, "v".into(), None, "video", Some(tx), false);
        q.next();
        assert!(matches!(q.stop(video), Stopped::Removed("video")));
        assert!(rx.recv().unwrap().is_err());
        assert_eq!(ids(&q), ["first"]);
    }

    #[test]
    fn stopping_a_running_run_finishes_the_image_in_flight() {
        let q = Queue::new();
        let id = q.push(Kind::Generate, "m".into(), None, "loop", None, false);
        q.next();
        assert!(matches!(q.stop(id), Stopped::Stopping));
        assert_eq!(q.snapshot().jobs[0].state, "stopping");
        assert!(!q.image_done(id));
        assert_eq!(q.len(), 0);
    }

    /// *Cancel generation* kills one image and the run goes on to the next;
    /// the cancelled image counts, so a pinned seed still walks past it.
    #[test]
    fn a_skipped_image_counts_and_the_run_goes_on() {
        let q = Queue::new();
        let id = q.push(Kind::Generate, "m".into(), Some(3), "batch", None, false);
        assert!(!q.skip(id), "not running yet: nothing in flight to cancel");
        q.next();
        assert!(q.skip(id));
        assert!(q.snapshot().jobs[0].skipping);
        assert!(q.skipped(id));
        assert!(q.image_done(id));
        assert!(!q.skipped(id), "the mark is that image's, not the next one's");
        assert_eq!(q.next().done, 1);

        // No child to kill: the mark is taken back, so a real failure still fails.
        assert!(q.skip(id));
        q.unskip(id);
        assert!(!q.skipped(id));

        let conv = q.push(Kind::Convert, "c".into(), None, "convert", None, true);
        assert!(!q.skip(conv), "an indivisible job has no next image");
    }

    #[test]
    fn a_running_indivisible_job_is_cancelled_not_removed() {
        let q = Queue::new();
        let id = q.push(Kind::Convert, "c".into(), None, "convert", None, false);
        q.next();
        assert!(matches!(q.stop(id), Stopped::Cancel));
        assert_eq!(q.len(), 1);
        q.finish(id, Err(anyhow!("cancelled")));
        assert_eq!(q.len(), 0);
    }

    /// A Redraw is one pass: a Stop kills it, it never steps between images.
    #[test]
    fn a_redraw_is_indivisible_and_named_on_the_wire() {
        assert!(!Kind::Redraw.is_run());
        assert!(!Kind::Upscale.is_run());
        assert_eq!(serde_json::to_string(&Kind::Upscale).unwrap(), "\"upscale\"");
        assert_eq!(serde_json::to_string(&Kind::Redraw).unwrap(), "\"redraw\"");
        let q = Queue::new();
        let id = q.push(Kind::Redraw, "m".into(), None, "redraw", None, true);
        q.next();
        assert!(matches!(q.stop(id), Stopped::Cancel));
    }

    #[test]
    fn a_failed_run_leaves_a_notice() {
        let q = Queue::new();
        let id = q.push(Kind::Img2img, "m".into(), Some(5), "run", None, false);
        q.next();
        q.fail_run(id, "out of memory".into());
        let snap = q.snapshot();
        assert!(snap.jobs.is_empty());
        assert_eq!(snap.notices[0].message, "out of memory");
    }

    #[test]
    fn the_seed_walks_only_when_pinned() {
        assert_eq!(walk_seed(100, 0), 100);
        assert_eq!(walk_seed(100, 3), 103);
        assert_eq!(walk_seed(-1, 3), -1);
    }
}
