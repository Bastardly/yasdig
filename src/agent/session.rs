//! The agent's bounded workflow, as pure functions over a [`Chat`]: what the
//! next step is, what a reply does to the chat.
//! The worker (`agent::step`) only carries them out — a turn, an image, a
//! pass — so the decisions are tested here without a GPU.
//!
//! A session runs **plan → round → critique → round → … → done**, at most
//! `rounds` rounds, and a plain chat turn may start the same loop by asking to
//! generate. Each step is one job-queue step, so a button press on the Image
//! tab runs between any two of them.
//!
//! **The agent sizes its own work** from what the person wrote — pictures in
//! a round, rounds in a session — under the caps in code
//! ([`knobs::MAX_BATCH`], [`knobs::MAX_ROUNDS`]). A chat round is one picture
//! unless asked; a session the plan didn't size is [`SESSION_ROUNDS`] of
//! [`SESSION_BATCH`].

use super::chat::{
    ActionRec, ActionState, Change, ChangeState, Chat, CheckRow, Film, FilmShot, ImageRef, Member, MemberKind, Mode, Msg, Next, Phase,
    State, Sweep,
};
use super::knobs::{self, Locks, MAX_BATCH, MAX_ROUNDS};
use super::toolkit::Toolkit;
use serde::Deserialize;
use serde_json::{Map, Value};

/// One step of a leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Turn(Mode),
    /// Picture `k` (0-based) of round `round`.
    Image { round: u32, k: u32 },
    Escalate(u64),
    /// The film's cast member `k` gets its picture: drawn, or the person's
    /// saved to Outputs.
    CastPicture(usize),
    /// Character `k`'s turnaround sheet is drawn.
    CastSheet(usize),
    /// Shot `k`'s keyframe is drawn.
    Keyframe(usize),
    /// Shot `k` is filmed: shot 1 makes the clip, a later one extends it,
    /// a retake redoes it.
    Shot(usize),
    End,
}

pub fn next_step(chat: &Chat) -> Step {
    let s = &chat.session;
    match s.next {
        Next::Nothing => Step::End,
        Next::Turn => Step::Turn(s.mode),
        Next::Critique => Step::Turn(Mode::Critique),
        Next::Batch => {
            let drawn = current_batch(chat).map_or(0, |(_, n, _)| n);
            Step::Image { round: s.round, k: drawn }
        }
        Next::Escalate => chat
            .actions
            .iter()
            .find(|a| a.state == ActionState::Queued)
            .map_or(Step::End, |a| Step::Escalate(a.id)),
        Next::Film => chat.film.as_ref().and_then(board_step).unwrap_or(Step::End),
        Next::Shoot => chat.film.as_ref().filter(|f| f.shot < f.shots.len()).map_or(Step::End, |f| Step::Shot(f.shot)),
        Next::Take => Step::Turn(Mode::Take),
    }
}

/// The storyboard's next missing picture: the cast first, since a keyframe
/// is drawn with their pictures.
///
/// A character with a sheet: a drawn one's sheet comes first and its front
/// picture is drawn from it; the person's picture is saved first and the
/// sheet drawn from it.
pub fn board_step(film: &Film) -> Option<Step> {
    for (k, m) in film.cast.iter().enumerate() {
        let sheet = film.wants_sheet(m) && m.sheet_stem.is_none();
        match (m.theirs(), m.stem.is_none(), sheet) {
            (true, true, _) | (false, true, false) => return Some(Step::CastPicture(k)),
            (_, _, true) => return Some(Step::CastSheet(k)),
            _ => {}
        }
    }
    (0..film.shots.len()).find(|&k| film.needs_keyframe(k) && film.shots[k].key_stem.is_none()).map(Step::Keyframe)
}

/// Whether a film may be shot: its board whole and ready, or its filming
/// stopped part way, to go on.
pub fn can_go(f: &Film) -> bool {
    match f.phase {
        Phase::Ready => board_step(f).is_none(),
        Phase::Shooting => f.shot < f.shots.len(),
        _ => false,
    }
}

/// The person's go (the Story panel's *Film*, or "go" in the chat): film the
/// shots from the first, or from where a stop left it. `watch`: the agent
/// model sees, so each shot is looked at.
pub fn go(chat: &mut Chat, watch: bool) -> Result<(), String> {
    let f = chat.film.as_mut().ok_or("this chat has no storyboard")?;
    if !can_go(f) {
        return Err(match f.phase {
            Phase::Board => "the storyboard still has pictures to draw: press Draw the cast first".into(),
            Phase::Done => "this film is done; ask for a new storyboard to make another".into(),
            _ => "nothing is left to film".into(),
        });
    }
    if f.phase == Phase::Ready {
        f.shot = 0;
        f.clip = None;
        for s in &mut f.shots {
            s.takes = 0;
            s.seed = 0;
            s.checks.clear();
        }
    }
    f.phase = Phase::Shooting;
    f.watch = watch;
    chat.session.next = Next::Shoot;
    Ok(())
}

/// What shot `k` of the film is sent to the video model as: its words, a
/// seed of its own, the model's length. A cut — and shot 1 — starts from its
/// keyframe on a model without references (shot 1's is the clip's source,
/// not on the spec), and from the pictures of the cast it names on one with
/// them (front pictures: a turnaround's four figures may read as four people).
pub fn shot_spec(f: &Film, k: usize, seed: i64) -> crate::pipeline::ShotSpec {
    let s = &f.shots[k];
    let fresh = k == 0 || s.cut;
    let references = if fresh && f.references {
        s.cast.iter().filter_map(|n| f.member(n)?.stem.clone()).take(crate::config::MAX_REFERENCES).collect()
    } else {
        Vec::new()
    };
    crate::pipeline::ShotSpec {
        text: s.text.clone(),
        seed,
        frames: None,
        cut: k > 0 && s.cut,
        references,
        keyframe: if k > 0 && s.cut && !f.references { s.key_stem.clone() } else { None },
    }
}

/// Shot `k` is filmed, into clip `clip` on `seed`: looked at next when the
/// agent can see, else the next shot.
pub fn shot_done(chat: &mut Chat, k: usize, seed: i64, clip: &str) {
    let Some(f) = chat.film.as_mut() else { return };
    f.clip = Some(clip.to_string());
    if let Some(s) = f.shots.get_mut(k) {
        s.seed = seed;
        s.takes += 1;
        s.checks.clear();
    }
    if f.watch {
        chat.session.next = Next::Take;
    } else {
        next_shot(chat);
    }
}

/// The yes/no questions a take is judged by: the server's, the same for
/// every film, so a small model can't set itself an easy test.
pub fn take_checks(f: &Film, k: usize) -> Vec<String> {
    let Some(s) = f.shots.get(k) else { return Vec::new() };
    let mut out = vec![format!("Does the shot show what it should: \"{}\"?", s.text)];
    let pictured: Vec<&str> = take_cast(f, k).iter().map(|m| m.name.as_str()).collect();
    if !pictured.is_empty() {
        out.push(format!("Do {} look like their pictures?", pictured.join(" and ")));
    }
    if k > 0 && !s.cut {
        out.push("Does it carry on from the frame before it: the same people, place and light?".into());
    }
    out
}

/// The cast a take is shown beside the shot: its first two with a picture,
/// as they should look.
pub fn take_cast(f: &Film, k: usize) -> Vec<&Member> {
    f.shots.get(k).map_or(Vec::new(), |s| s.cast.iter().filter_map(|n| f.member(n).filter(|m| m.picture.is_some())).take(2).collect())
}

/// The film moves on: the next shot, or done — its clip in the chat, and
/// which shots were left failing a check.
fn next_shot(chat: &mut Chat) {
    let Some(f) = chat.film.as_mut() else { return };
    f.shot += 1;
    if f.shot < f.shots.len() {
        chat.session.next = Next::Shoot;
        return;
    }
    f.phase = Phase::Done;
    chat.session.next = Next::Nothing;
    let (id, title, watch) = (f.clip.clone().unwrap_or_default(), f.title.clone(), f.watch);
    let failing: Vec<String> =
        f.shots.iter().enumerate().filter(|(_, s)| s.checks.contains(&false)).map(|(k, s)| format!("{} ({} takes)", k + 1, s.takes)).collect();
    let n = chat.next_n();
    chat.messages.push(Msg::Clip { n, ms: super::chat::now_ms(), id, title });
    if !watch {
        chat.note("No shot was checked: the agent model can't see pictures.");
    } else if !failing.is_empty() {
        chat.note(format!(
            "Still failing a check after a retake: shot {}. Redo from that shot on the Video tab, or ask for another take.",
            failing.join(", ")
        ));
    }
}

/// After the board changed: with `draw`, draw what it is missing; without,
/// leave that to the person (the Story panel's *Draw the cast*), who may
/// want to read the looks first. With nothing missing it is ready and waits
/// for the person's go.
pub fn board_advance(chat: &mut Chat, shot_seconds: f32, draw: bool) {
    let Some(film) = chat.film.as_mut() else { return };
    if !matches!(film.phase, Phase::Board | Phase::Ready) {
        return;
    }
    if board_step(film).is_some() {
        film.phase = Phase::Board;
        chat.session.next = if draw { Next::Film } else { Next::Nothing };
        return;
    }
    let was = film.phase;
    film.phase = Phase::Ready;
    chat.session.next = Next::Nothing;
    let n = film.shots.len();
    if was == Phase::Board {
        let secs = (n as f32 * shot_seconds).round();
        chat.note(format!(
            "The storyboard is ready: {n} shot{}, about {secs} s of video. Say go, or press Film.",
            if n == 1 { "" } else { "s" }
        ));
    }
}

/// A picture of the board landed: member or shot `k`'s, kept in the chat
/// (`picture`) and saved to Outputs (`stem`).
pub fn board_picture_done(chat: &mut Chat, step: Step, picture: ImageRef, stem: String, shot_seconds: f32) {
    let Some(film) = chat.film.as_mut() else { return };
    match step {
        Step::CastPicture(k) => {
            if let Some(m) = film.cast.get_mut(k) {
                m.picture = Some(picture);
                m.stem = Some(stem);
            }
        }
        Step::CastSheet(k) => {
            if let Some(m) = film.cast.get_mut(k) {
                m.sheet = Some(picture);
                m.sheet_stem = Some(stem);
            }
        }
        Step::Keyframe(k) => {
            if let Some(s) = film.shots.get_mut(k) {
                s.keyframe = Some(picture);
                s.key_stem = Some(stem);
            }
        }
        _ => return,
    }
    board_advance(chat, shot_seconds, true);
}

/// The round being drawn: its number, how many of its pictures exist, and
/// how many it will hold.
fn current_batch(chat: &Chat) -> Option<(u32, u32, u32)> {
    chat.messages.iter().rev().find_map(|m| match m {
        Msg::Batch { round, images, size, .. } if *round == chat.session.round => {
            Some((*round, images.len() as u32, whole(*size)))
        }
        _ => None,
    })
}

/// A session's rounds and their size when its plan didn't say.
pub const SESSION_ROUNDS: u32 = 3;
pub const SESSION_BATCH: u32 = 4;

/// A round's size as recorded: 0 in chats from before a round was sized,
/// which drew four.
fn whole(size: u32) -> u32 {
    if size == 0 { SESSION_BATCH } else { size }
}

/// Pictures per round of the session, and its rounds.
fn per_round(chat: &Chat) -> u32 {
    if chat.session.per_round == 0 { SESSION_BATCH } else { chat.session.per_round.min(MAX_BATCH) }
}

pub fn session_limit(chat: &Chat) -> u32 {
    if chat.session.rounds == 0 { SESSION_ROUNDS } else { chat.session.rounds.min(MAX_ROUNDS) }
}

/// How many pictures the current round holds when whole.
pub fn round_size(chat: &Chat) -> u32 {
    current_batch(chat).map_or(per_round(chat), |(_, _, size)| size)
}

/// The newest round, when it is whole and nothing was drawn after it: what a
/// new session judges first instead of drawing again. Its number and size.
pub fn judgeable(chat: &Chat) -> Option<(u32, u32)> {
    let (round, n, size) = current_batch(chat)?;
    (round > 0 && n >= size).then_some((round, size))
}

/// The seed for picture `k` of the round. Every round's seeds are drawn once
/// when it begins (`start_round`) and recorded, so a round can be drawn again.
pub fn seed_for(chat: &Chat, k: u32) -> i64 {
    chat.session.base_seed + k as i64
}

/// How many of its rounds the session has drawn.
pub fn session_rounds(chat: &Chat) -> u32 {
    chat.session.round.saturating_sub(chat.session.first_round)
}

/// Begin the next round: a `Batch` message with no pictures yet. Its seeds are
/// the last round's unless that round's are asked to be new (`fresh`) or there
/// was none — a session with the seed locked keeps them, so a difference
/// between rounds is the change; a chat turn says which it wants.
pub fn start_round(chat: &mut Chat, random_seed: i64) {
    if chat.session.base_seed == 0 || chat.session.fresh {
        chat.session.base_seed = random_seed.max(1);
    }
    let size = match chat.session.count {
        0 => per_round(chat),
        c => c.min(MAX_BATCH),
    };
    chat.session.count = 0;
    chat.session.fresh = false;
    // A comparison's round draws its own value, written before the round
    // records its settings. Only while the session runs: a chat round after
    // it draws what the chat holds.
    // Its value leads the round's line, which otherwise starts with a prompt.
    let mut drew = None;
    if chat.session.mode != Mode::Chat {
        let at = session_rounds(chat) as usize;
        if let Some((k, v)) = chat.session.sweep.as_ref().and_then(|s| Some((knobs::knob(&s.knob)?, s.values.get(at)?.clone()))) {
            knobs::write(&mut chat.settings, k, &v);
            chat.settings_rev += 1;
            drew = Some(knobs::value_label(k.key, &v));
        }
    }
    chat.session.round += 1;
    let n = chat.next_n();
    chat.messages.push(Msg::Batch {
        n,
        ms: super::chat::now_ms(),
        round: chat.session.round,
        settings_rev: chat.settings_rev,
        summary: match drew {
            Some(d) => format!("{d} · {}", summary(&chat.settings)),
            None => summary(&chat.settings),
        },
        size,
        images: Vec::new(),
    });
    chat.session.next = Next::Batch;
}

/// Whether the leg's next step is a picture of a round that hasn't begun —
/// none drawn yet, or the last one full — so the worker must begin it first.
pub fn needs_round(chat: &Chat) -> bool {
    chat.session.next == Next::Batch && current_batch(chat).is_none_or(|(_, n, size)| n >= size)
}

/// A round's settings in a line, for later turns that don't see its pictures.
pub fn summary(settings: &Value) -> String {
    let get = |k: &str| settings.get(k).filter(|v| !v.is_null());
    let mut parts = Vec::new();
    if let Some(p) = get("prompt").and_then(|v| v.as_str()) {
        let short: String = p.chars().take(160).collect();
        parts.push(format!("prompt \"{short}{}\"", if short.len() < p.len() { "…" } else { "" }));
    }
    if let Some(n) = get("negative").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        parts.push(format!("negative \"{}\"", n.chars().take(80).collect::<String>()));
    }
    for (k, label) in [("cfg_scale", "CFG"), ("steps", "steps")] {
        if let Some(v) = get(k) {
            parts.push(format!("{label} {v}"));
        }
    }
    if let (Some(w), Some(h)) = (get("width"), get("height")) {
        parts.push(format!("{w}×{h}"));
    }
    if let Some(m) = get("template").and_then(|v| v.as_str()) {
        parts.push(format!("model {m}"));
    }
    parts.join(", ")
}

/// The agent's words for what round `round` tries: the reason of a change it
/// made just before, else what it told the person. Empty when it said nothing
/// (a round drawn on the person's own settings).
pub fn round_reason(chat: &Chat, round: u32) -> String {
    let at = chat.messages.iter().position(|m| matches!(m, Msg::Batch { round: r, .. } if *r == round));
    let Some(at) = at else { return String::new() };
    chat.messages[..at]
        .iter()
        .rev()
        .take_while(|m| !matches!(m, Msg::Batch { .. }))
        .find_map(|m| match m {
            Msg::Agent { changes, reply, .. } => {
                let reason = chat
                    .changes
                    .iter()
                    .filter(|c| changes.contains(&c.id) && c.state == ChangeState::Applied)
                    .map(|c| c.reason.trim())
                    .find(|r| !r.is_empty());
                Some(reason.unwrap_or(reply.trim()).to_string())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// A picture of the round landed.
pub fn image_done(chat: &mut Chat, img: super::chat::ImageRef) {
    let round = chat.session.round;
    // A round a chat turn asked for is shown and left to the person, even
    // with a session's checks still on the chat: the person is steering now.
    let judge = !chat.session.checks.is_empty() && chat.session.mode != Mode::Chat;
    for m in chat.messages.iter_mut().rev() {
        if let Msg::Batch { round: r, images, size, .. } = m {
            if *r == round {
                images.push(img);
                let size = whole(*size);
                if images.len() as u32 >= size {
                    chat.session.next = if judge { Next::Critique } else { Next::Nothing };
                }
                return;
            }
        }
    }
}

/// A reply as the schema shapes it. Every field optional: the server reads
/// what it can and the grammar is not trusted to have delivered all of it.
#[derive(Debug, Default, Deserialize)]
pub struct ReplyJson {
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub reply: String,
    #[serde(default)]
    pub change: Option<Map<String, Value>>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub action: Option<String>,
    /// Plan: how many rounds the session runs.
    #[serde(default)]
    pub rounds: Option<u32>,
    /// How many pictures to draw (a plan: per round), and whether to keep
    /// the last round's seeds.
    #[serde(default)]
    pub count: Option<u32>,
    #[serde(default)]
    pub seeds: Option<String>,
    /// Plan: the questions. Critique: one row of answers per picture.
    #[serde(default)]
    pub checks: Option<Value>,
    #[serde(default)]
    pub keepers: Vec<u32>,
    #[serde(default)]
    pub escalate: Option<EscalateJson>,
    #[serde(default)]
    pub prompt: Option<String>,
    /// Chat, action "improve" or "compare": what the pictures must show, or
    /// what is being found out.
    #[serde(default)]
    pub goal: Option<String>,
    /// Chat, action "compare": the knob, and its values as written.
    #[serde(default)]
    pub vary: Option<String>,
    #[serde(default)]
    pub values: Option<Vec<Value>>,
    /// Verdict: the compared value that answered best, from 1.
    #[serde(default)]
    pub best: Option<u32>,
    #[serde(default)]
    pub negative: Option<String>,
    /// Take: an answer per check, and the shot's words rewritten for a
    /// retake ("" to keep them).
    #[serde(default)]
    pub answers: Option<Vec<bool>>,
    #[serde(default)]
    pub better_text: Option<String>,
    /// Story: the film's title, its look, the image model its pictures are
    /// drawn with, cast and shots.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub image_model: Option<String>,
    #[serde(default)]
    pub every: Option<String>,
    #[serde(default)]
    pub cast: Option<Vec<CastJson>>,
    #[serde(default)]
    pub shots: Option<Vec<ShotJson>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CastJson {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub look: String,
    /// The attached picture it is, from 1; 0 is none (drawn from `prompt`).
    #[serde(default)]
    pub picture: u32,
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct ShotJson {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub cast: Vec<String>,
    #[serde(default)]
    pub cut: bool,
}

/// The most cast members a film has: each is a picture drawn before the
/// board is ready, and a shot shows at most `MAX_REFERENCES` of them.
pub const MAX_CAST: usize = 6;

/// A storyboard from a Story reply, **written whole**: a small model
/// rewrites a board more reliably than it patches one. What is kept from
/// the board before is matched, not trusted to the model: a member keeps its
/// picture when its name is the same and its look is too — or the picture
/// is the person's, which the look describes — and a shot keeps its
/// keyframe when its words, cast and cut are unchanged.
/// Shot `k` made a cut when it goes on from `before`'s last shot but names
/// someone that shot doesn't: a continue starts from its last frame and
/// nothing else, so they would be drawn from words alone, where a cut starts
/// from their picture (H3's references, Wan's keyframe drawn with the cast).
/// Its number when it was made one.
fn recut(before: &[FilmShot], shot: &mut FilmShot, k: usize) -> Option<usize> {
    let prev = before.get(k.checked_sub(1)?)?;
    if shot.cut || shot.cast.iter().all(|n| prev.cast.contains(n)) {
        return None;
    }
    shot.cut = true;
    shot.keyframe = None;
    shot.key_stem = None;
    Some(k + 1)
}

/// Every shot of the board checked by [`recut`], in order — an edit to one
/// shot's cast can make the next one a cut. The note saying which, if any.
pub fn recut_board(film: &mut Film) -> Option<String> {
    let mut made = Vec::new();
    for k in 1..film.shots.len() {
        let (before, rest) = film.shots.split_at_mut(k);
        if let Some(n) = recut(before, &mut rest[0], k) {
            made.push(n);
        }
    }
    recut_note(&made)
}

fn recut_note(shots: &[usize]) -> Option<String> {
    let list: Vec<String> = shots.iter().map(usize::to_string).collect();
    (!list.is_empty()).then(|| {
        format!(
            "Shot {} brings in someone who isn't in the shot before, so it is a cut: a shot that goes on keeps only \
             what the last frame shows, and a cut starts from their picture.",
            list.join(", ")
        )
    })
}

pub fn apply_story(chat: &mut Chat, r: &mut ReplyJson, t: &Toolkit, notes: &mut Vec<String>) {
    let Some(video) = t.video.clone().filter(|_| t.films()) else {
        notes.push(format!("No storyboard: {}.", t.film_off.as_deref().unwrap_or("no video model")));
        return;
    };
    let attached: Vec<ImageRef> = chat
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            Msg::User { images, .. } => Some(images.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let old = chat.film.take();
    let mut cast: Vec<Member> = Vec::new();
    for c in r.cast.take().unwrap_or_default() {
        let name: String = plain(&c.name).split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() || cast.iter().any(|m| m.name.eq_ignore_ascii_case(&name)) {
            continue;
        }
        if cast.len() == MAX_CAST {
            notes.push(format!("A film has at most {MAX_CAST} in its cast, so {name} was left out."));
            continue;
        }
        let look = plain(&c.look);
        let mut m = Member {
            name,
            kind: match c.kind.as_str() {
                "place" => MemberKind::Place,
                "thing" => MemberKind::Thing,
                _ => MemberKind::Character,
            },
            look,
            prompt: plain(&c.prompt),
            ..Default::default()
        };
        let adopted = (c.picture as usize).checked_sub(1).and_then(|i| attached.get(i));
        if let Some(p) = adopted {
            // The person's picture: an output already is its own stem.
            m.stem = (p.kind == "output").then(|| p.id.clone());
            m.picture = Some(p.clone());
        } else if let Some(was) = old.as_ref().and_then(|f| f.member(&m.name)) {
            let theirs = was.picture.as_ref().is_some_and(|p| p.kind != "item");
            if theirs || was.look == m.look {
                m.picture = was.picture.clone();
                m.stem = was.stem.clone();
                m.sheet = was.sheet.clone();
                m.sheet_stem = was.sheet_stem.clone();
            }
        }
        cast.push(m);
    }
    let mut shots: Vec<FilmShot> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    let mut recuts: Vec<usize> = Vec::new();
    for s in r.shots.take().unwrap_or_default() {
        let text = plain(&s.text);
        if text.is_empty() {
            continue;
        }
        if shots.len() == crate::pipeline::MAX_SHOTS {
            notes.push(format!("A clip has at most {} shots, so the rest were left out.", crate::pipeline::MAX_SHOTS));
            break;
        }
        let mut names: Vec<String> = Vec::new();
        for n in &s.cast {
            match cast.iter().find(|m| m.name.eq_ignore_ascii_case(n.trim())) {
                Some(m) if !names.contains(&m.name) => names.push(m.name.clone()),
                Some(_) => {}
                None if !n.trim().is_empty() && !unknown.iter().any(|u| u.eq_ignore_ascii_case(n.trim())) => {
                    unknown.push(n.trim().to_string())
                }
                None => {}
            }
        }
        names.truncate(crate::config::MAX_REFERENCES);
        let k = shots.len();
        let mut shot = FilmShot { text, cast: names, cut: k > 0 && s.cut, ..Default::default() };
        if let Some(n) = recut(&shots, &mut shot, k) {
            recuts.push(n);
        }
        if let Some(was) = old.as_ref().and_then(|f| f.shots.get(k)) {
            if was.text == shot.text && was.cast == shot.cast && was.cut == shot.cut {
                shot.keyframe = was.keyframe.clone();
                shot.key_stem = was.key_stem.clone();
            }
        }
        shots.push(shot);
    }
    if !unknown.is_empty() {
        notes.push(format!("The shots named {}, who aren't in the cast; left out of them.", unknown.join(", ")));
    }
    if let Some(n) = recut_note(&recuts) {
        notes.push(n);
    }
    if shots.is_empty() {
        notes.push("The storyboard had no shots, so the board is as it was.".into());
        chat.film = old;
        return;
    }
    let image = pick_image_model(chat, r.image_model.as_deref(), old.as_ref(), t);
    let sheets = t.models.iter().any(|m| m.name == image && m.references);
    if !sheets && cast.iter().any(|m| m.kind == MemberKind::Character) {
        notes.push(format!(
            "The cast is drawn with {image}, which can't follow a reference picture, so each character gets one \
             picture rather than a turnaround from every side. A FLUX.2 or FLUX.3 Klein can."
        ));
    }
    let title = r.title.take().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    chat.film = Some(Film {
        title: title.or_else(|| old.as_ref().map(|f| f.title.clone())).unwrap_or_default(),
        every: plain(&r.every.take().unwrap_or_default()),
        cast,
        shots,
        video_model: video.name.clone(),
        references: video.references,
        image_model: image,
        sheets,
        watch: false,
        clip: None,
        phase: Phase::Board,
        shot: 0,
    });
    board_advance(chat, video.shot_seconds(), false);
    if chat.film.as_ref().is_some_and(|f| f.phase == Phase::Board) {
        notes.push("The storyboard is written. Read the cast in the Story panel, change what you like, then press Draw the cast.".into());
    }
}

/// The image model a board's pictures are drawn with: the agent's pick when
/// it may pick a model and named an installed one, else the board's before,
/// else the chat's.
fn pick_image_model(chat: &Chat, picked: Option<&str>, old: Option<&Film>, t: &Toolkit) -> String {
    let installed = |n: &str| t.models.iter().any(|m| m.name == n);
    let may = t.knob("model").is_some_and(|k| k.allowed);
    picked
        .filter(|n| may && installed(n))
        .map(str::to_string)
        .or_else(|| old.map(|f| f.image_model.clone()).filter(|n| installed(n)))
        .unwrap_or_else(|| chat.settings.get("template").and_then(|v| v.as_str()).unwrap_or("").to_string())
}

/// A take judged: one retake on a new seed when a check failed and the shot
/// has had only one, with the agent's better words if it wrote some. `true`
/// when the film moves on instead, which the caller does after recording
/// the reply, so the clip comes after it in the chat.
fn take(chat: &mut Chat, r: &ReplyJson, notes: &mut Vec<String>) -> bool {
    let Some(f) = chat.film.as_mut() else { return false };
    let k = f.shot;
    let want = take_checks(f, k).len();
    let Some(s) = f.shots.get_mut(k) else { return false };
    let mut answers: Vec<bool> = r.answers.clone().unwrap_or_default().into_iter().take(want).collect();
    // An answer the grammar didn't deliver is no pass.
    answers.resize(want, false);
    s.checks = answers;
    if s.checks.iter().all(|b| *b) || s.takes >= 2 {
        return true;
    }
    let better = plain(r.better_text.as_deref().unwrap_or(""));
    if !better.is_empty() && better != s.text {
        notes.push(format!("Shot {} again, as: {better}", k + 1));
        s.text = better;
    } else {
        notes.push(format!("Shot {} again, on a new seed.", k + 1));
    }
    chat.session.next = Next::Shoot;
    false
}

/// A small model writes Markdown whatever it is told; in a look or a prompt
/// the stars would be drawn as words.
fn plain(s: &str) -> String {
    s.replace("**", "").replace("__", "").trim().to_string()
}

#[derive(Debug, Default, Deserialize)]
pub struct EscalateJson {
    pub image: u32,
    pub pass: String,
    #[serde(default)]
    pub instruction: Option<String>,
}

/// Read a reply. The template's parser already split off any thinking, so
/// what is left should be the object; a reply wrapped in a code fence or with
/// words around it is read from its outermost braces.
pub fn parse_reply(text: &str) -> anyhow::Result<ReplyJson> {
    let t = text.trim();
    if let Ok(r) = serde_json::from_str(t) {
        return Ok(r);
    }
    let (a, b) = (t.find('{'), t.rfind('}'));
    match (a, b) {
        (Some(a), Some(b)) if a < b => serde_json::from_str(&t[a..=b])
            .map_err(|e| anyhow::anyhow!("the agent's reply isn't the JSON it was asked for ({e})")),
        _ => anyhow::bail!("the agent's reply isn't the JSON it was asked for"),
    }
}

/// Start a session on `goal`: its own checks (the plan turn writes them),
/// its rounds counted from here. Round numbers go on, so none is reused. The
/// plan decides whether it judges the round just drawn or draws afresh. The
/// pictures attached to the message it answers are what it is judged against.
pub fn begin(chat: &mut Chat, goal: &str) {
    chat.session.goal = goal.to_string();
    chat.session.checks.clear();
    chat.session.first_round = chat.session.round;
    chat.session.rounds = 0;
    chat.session.per_round = 0;
    chat.session.sweep = None;
    chat.session.reference = chat
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            Msg::User { images, .. } => Some(images.clone()),
            _ => None,
        })
        .unwrap_or_default();
    chat.session.mode = Mode::Plan;
}

/// A comparison from a "compare" reply: each value read, checked as a change
/// to it would be (whitelist, locks, bounds) and kept once. `None`, with the
/// reasons in `notes`, unless two values are left.
fn sweep_from(chat: &Chat, r: &ReplyJson, t: &Toolkit, locks: &Locks, notes: &mut Vec<String>) -> Option<Sweep> {
    let Some(key) = r.vary.as_deref().filter(|k| knobs::COMPARABLE.contains(k)) else {
        notes.push("The agent wanted to compare but named no setting it can compare.".into());
        return None;
    };
    let k = knobs::knob(key)?;
    let models = t.model_names();
    let mut values = Vec::new();
    for raw in r.values.iter().flatten() {
        let v = knobs::parse_value(key, raw);
        if let Some(v) = knobs::check(key, &v, &t.rules, locks, &models, notes) {
            if !values.contains(&v) {
                values.push(v);
            }
        }
    }
    values.truncate(MAX_ROUNDS as usize);
    if values.len() < 2 {
        notes.push(format!("A comparison needs two values of {} the agent may use; it had {}.", k.label, values.len()));
        return None;
    }
    Some(Sweep { knob: key.to_string(), values, before: knobs::value_of(&chat.settings, k) })
}

/// The critique of round `round`: its answer rows.
fn critique_of(chat: &Chat, round: u32) -> Option<&[CheckRow]> {
    let at = chat.messages.iter().position(|m| matches!(m, Msg::Batch { round: r, .. } if *r == round))?;
    chat.messages[at + 1..].iter().find_map(|m| match m {
        Msg::Agent { mode: Mode::Critique, checks, .. } => Some(checks.as_slice()),
        _ => None,
    })
}

/// A comparison's results, counted by the server from the critiques'
/// answers: the checks, then a numbered line per value. What the verdict
/// turn is shown and what the person reads, whatever the model makes of it.
pub fn sweep_table(chat: &Chat) -> String {
    let Some(sw) = &chat.session.sweep else { return String::new() };
    let label = knobs::knob(&sw.knob).map_or(sw.knob.as_str(), |k| k.label);
    let mut lines = vec![format!("Comparing {label}, the same seeds each time. Checks:")];
    for (i, c) in chat.session.checks.iter().enumerate() {
        lines.push(format!("  {}. {c}", i + 1));
    }
    for (i, v) in sw.values.iter().enumerate() {
        let what = knobs::value_label(&sw.knob, v);
        let Some(rows) = critique_of(chat, chat.session.first_round + 1 + i as u32) else {
            lines.push(format!("{}. {what}: not drawn", i + 1));
            continue;
        };
        let want = chat.session.checks.len();
        let all = rows.iter().filter(|r| r.results.len() == want && r.results.iter().all(|b| *b)).count();
        let per: Vec<String> = (0..want)
            .map(|c| format!("{} {}/{}", c + 1, rows.iter().filter(|r| r.results.get(c) == Some(&true)).count(), rows.len()))
            .collect();
        lines.push(format!("{}. {what}: {all} of {} passed every check ({})", i + 1, rows.len(), per.join(", ")));
    }
    lines.join("\n")
}

/// Record a reply on the chat and decide what comes next.
pub fn apply_reply(chat: &mut Chat, mode: Mode, mut r: ReplyJson, t: &Toolkit, locks: &Locks, skills: Vec<String>, stats: crate::llm::Stats) {
    let n = chat.next_n();
    let mut change_ids = Vec::new();
    let mut notes_out = Vec::new();
    // A verdict puts the knob back as it was before the comparison, then sets
    // the best value as the agent's change, so Undo returns to the start.
    if mode == Mode::Verdict {
        if let Some(sw) = chat.session.sweep.clone() {
            if let Some(k) = knobs::knob(&sw.knob) {
                knobs::write(&mut chat.settings, k, &sw.before);
                chat.settings_rev += 1;
            }
            match r.best.and_then(|b| sw.values.get((b as usize).checked_sub(1)?)) {
                Some(v) => {
                    r.change = Some(Map::from_iter([(sw.knob.clone(), v.clone())]));
                    if r.reason.trim().is_empty() {
                        r.reason = format!("the comparison's best: {}", knobs::value_label(&sw.knob, v));
                    }
                }
                None => notes_out.push("The comparison picked no value, so the setting is back to what it was.".into()),
            }
        }
    }
    // Settings: whatever is on the whitelist, at once.
    if let Some(change) = r.change.as_ref().filter(|c| !c.is_empty()) {
        let applied = knobs::apply(&mut chat.settings, change, &t.rules, locks, &t.model_names(), &|m| t.locks_of(m).cloned());
        notes_out.extend(applied.notes.iter().filter(|n| !n.contains("isn't the agent's to change")).cloned());
        if !applied.set.is_empty() {
            chat.settings_rev += 1;
            let id = chat.next_change_id();
            chat.changes.push(Change {
                id,
                by: "agent".into(),
                state: ChangeState::Applied,
                set: applied.set,
                before: applied.before,
                reason: r.reason.clone(),
                msg: n,
                notes: Vec::new(),
            });
            change_ids.push(id);
        }
    }

    let batch = current_batch(chat).map_or(0, |(_, n, _)| n);
    let mut checks_rows = Vec::new();
    let mut passed = Vec::new();
    let mut keepers = Vec::new();
    let mut action_id = None;
    let mut table = false;
    let mut film_on = false;
    match mode {
        Mode::Plan if chat.session.sweep.is_some() => {
            chat.session.checks = plan_checks(&r);
            // The server sized it: a round per value, the same seeds.
            chat.session.rounds = chat.session.sweep.as_ref().map_or(0, |s| s.values.len() as u32);
            chat.session.fresh = true;
            chat.session.next = Next::Batch;
        }
        Mode::Plan => {
            chat.session.checks = plan_checks(&r);
            // How long and how wide, as the goal said; unsaid is the default.
            chat.session.rounds = r.rounds.map_or(0, |n| n.clamp(1, MAX_ROUNDS));
            chat.session.per_round = r.count.map_or(0, |n| n.clamp(1, MAX_BATCH));
            // A round just drawn is judged first, unless the plan rewrote the
            // prompt (the change just made): then those pictures answer another goal.
            let rewrote = chat.changes.iter().any(|c| change_ids.contains(&c.id) && c.set.contains_key("prompt"));
            match judgeable(chat) {
                Some((round, _)) if !rewrote => {
                    chat.session.first_round = round - 1;
                    chat.session.next = Next::Critique;
                }
                _ => {
                    chat.session.fresh = true;
                    chat.session.next = Next::Batch;
                }
            }
        }
        Mode::Critique => {
            let rows: Vec<Vec<bool>> = r
                .checks
                .as_ref()
                .and_then(|c| serde_json::from_value(c.clone()).ok())
                .unwrap_or_default();
            let want = chat.session.checks.len().max(1);
            for (i, row) in rows.into_iter().take(batch as usize).enumerate() {
                let row: Vec<bool> = row.into_iter().take(want).collect();
                // The server's own ranking: every check yes. Kept beside the
                // model's keepers, since a small model flatters what it drew.
                if row.len() == want && row.iter().all(|b| *b) {
                    passed.push(i as u32 + 1);
                }
                checks_rows.push(CheckRow { image: i as u32 + 1, results: row });
            }
            keepers = r.keepers.iter().copied().filter(|k| (1..=batch).contains(k)).collect();
            keepers.dedup();
            let action = r.action.as_deref().unwrap_or("done");
            chat.session.next = match action {
                // A comparison's rounds are the server's: the next value, the
                // same seeds; after the last, the counts and a verdict.
                _ if chat.session.sweep.is_some() && session_rounds(chat) < session_limit(chat) => Next::Batch,
                _ if chat.session.sweep.is_some() => {
                    table = true;
                    chat.session.mode = Mode::Verdict;
                    Next::Turn
                }
                "generate" if session_rounds(chat) < session_limit(chat) => {
                    // The same seeds unless it asks for new ones, so a difference
                    // between rounds is the change.
                    chat.session.fresh = r.seeds.as_deref() == Some("new");
                    chat.session.count = r.count.map_or(0, |n| n.clamp(1, MAX_BATCH));
                    Next::Batch
                }
                "generate" => {
                    notes_out.push(format!("That was round {} of {}, so the session stops here.", session_rounds(chat), session_limit(chat)));
                    Next::Nothing
                }
                "escalate" => match escalation(chat, r.escalate.as_ref(), t, n) {
                    Ok(id) => {
                        action_id = Some(id);
                        Next::Escalate
                    }
                    Err(why) => {
                        notes_out.push(why);
                        Next::Nothing
                    }
                },
                _ => Next::Nothing,
            };
        }
        Mode::Chat => {
            chat.session.next = match r.action.as_deref() {
                // Each older name too, for a reply from before the rename.
                Some("draw" | "generate") => {
                    // One picture unless the person asked for more.
                    chat.session.count = r.count.unwrap_or(1).clamp(1, MAX_BATCH);
                    // New seeds unless asked to keep the composition.
                    chat.session.fresh = r.seeds.as_deref() != Some("same");
                    Next::Batch
                }
                // The leg goes on to the plan turn, which writes the checks
                // and sizes the session.
                Some("improve" | "session") => match r.goal.as_deref().map(str::trim).filter(|g| !g.is_empty()) {
                    Some(goal) => {
                        begin(chat, goal);
                        Next::Turn
                    }
                    None => {
                        notes_out.push("The agent wanted to run a session but gave it no goal.".into());
                        Next::Nothing
                    }
                },
                Some("compare") => {
                    let goal = r.goal.as_deref().map(str::trim).filter(|g| !g.is_empty()).unwrap_or("a comparison").to_string();
                    // Its change is in already: a switch of model brings that model's locks.
                    let model = chat.settings.get("template").and_then(|v| v.as_str()).unwrap_or("");
                    let locks = t.locks_of(model).filter(|_| change_ids.iter().any(|id| {
                        chat.changes.iter().any(|c| c.id == *id && c.set.contains_key("model"))
                    })).unwrap_or(locks);
                    match sweep_from(chat, &r, t, locks, &mut notes_out) {
                        Some(sw) => {
                            begin(chat, &goal);
                            chat.session.per_round = r.count.unwrap_or(2).clamp(1, MAX_BATCH);
                            chat.session.rounds = sw.values.len() as u32;
                            chat.session.sweep = Some(sw);
                            Next::Turn
                        }
                        None => Next::Nothing,
                    }
                }
                Some("go") => match go(chat, t.vision) {
                    Ok(()) => Next::Shoot,
                    Err(why) => {
                        notes_out.push(format!("Not filming: {why}."));
                        Next::Nothing
                    }
                },
                // The storyboard is a turn of its own (`Mode::Story`), whose
                // schema is the board: the chat's would be twice as long.
                Some("film") if t.films() => {
                    chat.session.mode = Mode::Story;
                    Next::Turn
                }
                Some("film") => {
                    notes_out.push(format!("No film: {}.", t.film_off.as_deref().unwrap_or("")));
                    Next::Nothing
                }
                // "describe" writes its prompt on the message, below.
                _ => Next::Nothing,
            };
        }
        Mode::Describe | Mode::Verdict => chat.session.next = Next::Nothing,
        Mode::Take => film_on = take(chat, &r, &mut notes_out),
        Mode::Story => {
            chat.session.next = Next::Nothing;
            chat.session.mode = Mode::Chat;
            apply_story(chat, &mut r, t, &mut notes_out);
        }
    }

    chat.messages.push(Msg::Agent {
        n,
        ms: super::chat::now_ms(),
        mode,
        reply: r.reply.trim().to_string(),
        notes: r.notes.trim().to_string(),
        checks: checks_rows,
        keepers,
        passed,
        changes: change_ids,
        action: action_id,
        prompt: r.prompt.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
        negative: r.negative.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
        skills,
        stats,
    });
    for note in notes_out {
        chat.note(note);
    }
    if table {
        let t = sweep_table(chat);
        chat.note(t);
    }
    if film_on {
        next_shot(chat);
    }
    // A new round is begun by the worker (it draws the seed), so a reply only
    // says "a round next"; the round itself starts when the leg goes on.
}

/// A plan's checks, trimmed, the empty ones dropped.
fn plan_checks(r: &ReplyJson) -> Vec<String> {
    r.checks
        .as_ref()
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|q| q.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .take(super::schema::MAX_CHECKS as usize)
        .collect()
}

/// The pass a critique asked for, queued.
fn escalation(chat: &mut Chat, e: Option<&EscalateJson>, t: &Toolkit, msg: u32) -> Result<u64, String> {
    let e = e.ok_or("the agent asked for a pass without saying which picture")?;
    let a = t
        .actions
        .iter()
        .find(|a| a.key == e.pass)
        .filter(|a| a.offered())
        .ok_or_else(|| format!("the agent asked for a {} pass, which it may not run", e.pass))?;
    let item = chat
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            Msg::Batch { round, images, .. } if *round == chat.session.round => images.get(e.image.checked_sub(1)? as usize).cloned(),
            _ => None,
        })
        .ok_or_else(|| format!("there is no picture {} in this round", e.image))?;
    let instruction = e.instruction.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if a.key == "edit" && instruction.is_none() {
        return Err("the agent asked for an Edit without saying what to change".into());
    }
    let id = chat.next_action_id();
    chat.actions.push(ActionRec {
        id,
        kind: a.key.to_string(),
        item: item.id,
        instruction,
        state: ActionState::Queued,
        result: None,
        msg,
        error: None,
    });
    Ok(id)
}

/// How a leg ends when nothing is next: a session is done, a chat is idle.
pub fn finished(chat: &Chat) -> State {
    if chat.session.goal.is_empty() { State::Idle } else { State::Done }
}

/// Undo one applied change: put each key back, provided it still holds what
/// the change set. Recorded as a change of the person's, so it can be undone
/// in turn.
pub fn undo(chat: &mut Chat, id: u64) -> Result<u64, String> {
    let c = chat
        .changes
        .iter()
        .find(|c| c.id == id && c.state == ChangeState::Applied)
        .cloned()
        .ok_or("that change isn't applied")?;
    for (key, after) in &c.set {
        let k = knobs::knob(key).ok_or("an unknown setting")?;
        if &knobs::value_of(&chat.settings, k) != after {
            return Err(format!("{} has changed since, so it was left as it is", k.label));
        }
    }
    for (key, before) in &c.before {
        if let Some(k) = knobs::knob(key) {
            knobs::write(&mut chat.settings, k, before);
        }
    }
    if let Some(orig) = chat.changes.iter_mut().find(|x| x.id == id) {
        orig.state = ChangeState::Undone;
    }
    chat.settings_rev += 1;
    let new = chat.next_change_id();
    chat.changes.push(Change {
        id: new,
        by: "user".into(),
        state: ChangeState::Applied,
        set: c.before.clone(),
        before: c.set.clone(),
        reason: format!("undid change {id}"),
        msg: chat.next_n().saturating_sub(1),
        notes: Vec::new(),
    });
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::super::chat::ImageRef;
    use super::super::knobs::Rules;
    use super::super::toolkit::{build, ModelChoice};
    use super::*;
    use crate::config::ImageFamily;
    use serde_json::json;

    fn chat() -> Chat {
        let mut c = Chat::new("1-0".into(), json!({"prompt": "a fox", "cfg_scale": 6.0}), None);
        c.session.per_round = 2;
        c.session.rounds = 2;
        c.session.goal = "a fox in a red hat".into();
        c.session.next = Next::Turn;
        c.session.mode = Mode::Plan;
        c
    }

    fn tk(c: &Chat) -> Toolkit {
        tk_under(c, &Rules::default())
    }

    fn tk_under(c: &Chat, rules: &Rules) -> Toolkit {
        build(rules, &c.settings, &Locks::default(), vec![], Some(ImageFamily::Sdxl))
    }

    fn img(id: &str) -> ImageRef {
        ImageRef { kind: "item".into(), id: id.into(), file: format!("{id}.png"), seed: Some(1) }
    }

    fn reply(v: Value) -> ReplyJson {
        serde_json::from_value(v).unwrap()
    }

    /// A toolkit that can make a film on a Wan 2.2 5B (cuts from keyframes),
    /// or on H3 (cuts from the cast's pictures).
    fn film_tk(c: &Chat, h3: bool) -> Toolkit {
        let mut t = tk(c);
        t.video = Some(super::super::toolkit::VideoView {
            name: if h3 { "h3" } else { "wan5b" }.into(),
            family: if h3 { "minimax_h3" } else { "wan22_ti2v_5b" },
            label: "a video model",
            chains: true,
            references: h3,
            width: 832,
            height: 480,
            frames: 121,
            fps: 24,
        });
        t.film_off = None;
        t
    }

    fn story(c: &mut Chat, t: &Toolkit, v: Value) {
        apply_reply(c, Mode::Story, reply(v), t, &Locks::default(), vec![], Default::default());
    }

    fn board() -> Value {
        json!({"notes": "", "title": "Rain", "every": "cinematic, warm light",
            "cast": [
                {"name": "Mara", "kind": "character", "look": "a tall woman in a red coat", "picture": 1, "prompt": "Mara standing"},
                {"name": "Leo", "kind": "character", "look": "a boy in a yellow raincoat", "picture": 0, "prompt": "Leo standing"},
                {"name": " mara ", "kind": "character", "look": "twice", "picture": 0, "prompt": ""}
            ],
            "shots": [
                {"text": "Mara opens the door.", "cast": ["Mara", "Bob"], "cut": true},
                {"text": "She steps into the rain.", "cast": ["mara"], "cut": false},
                {"text": "Leo waits at the bus stop.", "cast": ["Leo"], "cut": true},
                {"text": "  ", "cast": [], "cut": true}
            ],
            "reply": "A short film in three shots."})
    }

    fn film_chat() -> Chat {
        let mut c = Chat::new("1-0".into(), json!({"prompt": "x"}), None);
        let pic = ImageRef { kind: "output".into(), id: "mara-photo".into(), file: "u0-0.png".into(), seed: None };
        c.messages.push(Msg::User { n: 0, ms: 0, text: "A film. This is Mara.".into(), images: vec![pic] });
        c
    }

    #[test]
    fn a_chat_turn_hands_a_film_to_the_story_turn_only_where_one_can_be_made() {
        let mut c = film_chat();
        let t = film_tk(&c, false);
        apply_reply(&mut c, Mode::Chat, reply(json!({"notes": "", "action": "film", "reply": "On it."})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(next_step(&c), Step::Turn(Mode::Story));
        let mut c = film_chat();
        let t = tk(&c);
        apply_reply(&mut c, Mode::Chat, reply(json!({"notes": "", "action": "film", "reply": "On it."})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(next_step(&c), Step::End);
        assert!(matches!(c.messages.last(), Some(Msg::Note { text, .. }) if text.starts_with("No film")));
    }

    #[test]
    fn a_story_reply_builds_the_film_from_what_it_can_trust() {
        let mut c = film_chat();
        let t = film_tk(&c, false);
        story(&mut c, &t, board());
        let f = c.film.as_ref().expect("a film");
        assert_eq!((f.title.as_str(), f.video_model.as_str(), f.references), ("Rain", "wan5b", false));
        let names: Vec<&str> = f.cast.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Mara", "Leo"], "a name once, however it is spelt");
        assert_eq!(f.cast[0].stem.as_deref(), Some("mara-photo"), "the attached picture is Mara's, an output already");
        assert!(f.cast[1].picture.is_none());
        assert_eq!(f.shots.len(), 3, "an empty shot is no shot");
        assert_eq!(f.shots[0].cast, ["Mara"], "Bob isn't cast");
        assert!(!f.shots[0].cut, "shot 1 is never a cut");
        assert_eq!(f.shots[1].cast, ["Mara"], "named as the cast spells it");
        assert!(c.messages.iter().any(|m| matches!(m, Msg::Note { text, .. } if text.contains("Bob"))));
        assert_eq!(f.phase, Phase::Board);
    }

    #[test]
    fn a_continue_that_brings_someone_back_is_made_a_cut() {
        let mut c = film_chat();
        let t = film_tk(&c, false);
        let mut b = board();
        b["shots"] = json!([
            {"text": "Mara and Leo meet.", "cast": ["Mara", "Leo"], "cut": false},
            {"text": "Mara walks off alone.", "cast": ["Mara"], "cut": false},
            {"text": "Leo catches up with her.", "cast": ["Mara", "Leo"], "cut": false}
        ]);
        story(&mut c, &t, b);
        let cuts: Vec<bool> = c.film.as_ref().unwrap().shots.iter().map(|s| s.cut).collect();
        assert_eq!(cuts, [false, false, true], "leaving is a continue; coming back starts from his picture");
        assert!(c.messages.iter().any(|m| matches!(m, Msg::Note { text, .. } if text.starts_with("Shot 3 brings in"))));
    }

    #[test]
    fn an_edit_that_brings_someone_into_a_continue_makes_it_a_cut() {
        let mut c = film_chat();
        let t = film_tk(&c, false);
        story(&mut c, &t, board());
        let f = c.film.as_mut().unwrap();
        assert!(recut_board(f).is_none(), "the board as written needs none");
        f.shots[1].cast.push("Leo".into());
        assert!(recut_board(f).is_some_and(|n| n.starts_with("Shot 2 ")));
        assert!(f.shots[1].cut);
    }

    #[test]
    fn the_board_draws_the_cast_then_the_keyframes_its_model_needs_then_waits() {
        let mut c = film_chat();
        let t = film_tk(&c, false);
        story(&mut c, &t, board());
        // Written, and left for the person to read before anything is drawn.
        assert_eq!(next_step(&c), Step::End);
        assert!(matches!(c.messages.last(), Some(Msg::Note { text, .. }) if text.contains("Draw the cast")));
        board_advance(&mut c, 5.0, true);
        // Mara's picture is the person's; Leo's is drawn.
        assert_eq!(next_step(&c), Step::CastPicture(1));
        board_picture_done(&mut c, Step::CastPicture(1), img("leo"), "leo-s0".into(), 5.0);
        // On Wan, shot 1 and every cut start from a keyframe.
        assert_eq!(next_step(&c), Step::Keyframe(0));
        board_picture_done(&mut c, Step::Keyframe(0), img("k0"), "k0-s0".into(), 5.0);
        assert_eq!(next_step(&c), Step::Keyframe(2), "shot 2 goes on, so it needs none");
        board_picture_done(&mut c, Step::Keyframe(2), img("k2"), "k2-s0".into(), 5.0);
        assert_eq!(next_step(&c), Step::End);
        assert_eq!(c.film.as_ref().unwrap().phase, Phase::Ready);
        assert!(matches!(c.messages.last(), Some(Msg::Note { text, .. }) if text.contains("3 shots, about 15 s")));

        // On H3 a cut follows the cast's pictures: no keyframes at all.
        let mut c = film_chat();
        let t = film_tk(&c, true);
        story(&mut c, &t, board());
        board_advance(&mut c, 5.0, true);
        board_picture_done(&mut c, Step::CastPicture(1), img("leo"), "leo-s0".into(), 5.0);
        assert_eq!(c.film.as_ref().unwrap().phase, Phase::Ready);
    }

    #[test]
    fn on_a_model_that_follows_references_characters_get_a_turnaround_first() {
        let mut c = film_chat();
        c.settings = json!({"prompt": "x", "template": "sdxl"});
        let mut t = film_tk(&c, true);
        t.models = vec![
            ModelChoice { name: "sdxl".into(), family: "sdxl", label: "SDXL", references: false, locks: Locks::default() },
            ModelChoice { name: "klein".into(), family: "flux2-klein-4b", label: "FLUX.2 Klein 4B", references: true, locks: Locks::default() },
        ];
        let mut b = board();
        b["image_model"] = json!("klein");
        b["cast"][1]["kind"] = json!("place");
        b["cast"].as_array_mut().unwrap().push(json!({"name": "Ana", "kind": "character", "look": "a girl", "picture": 0, "prompt": "Ana"}));
        story(&mut c, &t, b.clone());
        let f = c.film.as_ref().unwrap();
        assert_eq!((f.image_model.as_str(), f.sheets), ("klein", true));
        board_advance(&mut c, 5.0, true);
        // Mara's picture is the person's: kept as it is, her sheet drawn from it.
        assert_eq!(next_step(&c), Step::CastSheet(0));
        board_picture_done(&mut c, Step::CastSheet(0), img("t0"), "t0-s0".into(), 5.0);
        // Leo is a place here: one picture, no sheet.
        assert_eq!(next_step(&c), Step::CastPicture(1));
        board_picture_done(&mut c, Step::CastPicture(1), img("leo"), "leo-s0".into(), 5.0);
        // Ana is drawn: her sheet first, her front picture from it.
        assert_eq!(next_step(&c), Step::CastSheet(2));
        board_picture_done(&mut c, Step::CastSheet(2), img("t2"), "t2-s0".into(), 5.0);
        assert_eq!(next_step(&c), Step::CastPicture(2));
        board_picture_done(&mut c, Step::CastPicture(2), img("ana"), "ana-s0".into(), 5.0);
        assert_eq!(c.film.as_ref().unwrap().phase, Phase::Ready, "H3: no keyframes");

        // Without the model knob the chat's model draws them, one picture each, and the chat says why.
        let mut no = Rules::default();
        no.knobs.remove("model");
        let mut c = film_chat();
        c.settings = json!({"prompt": "x", "template": "sdxl"});
        let t = Toolkit { knobs: tk_under(&c, &no).knobs, ..t };
        story(&mut c, &t, b);
        assert_eq!((c.film.as_ref().unwrap().image_model.as_str(), c.film.as_ref().unwrap().sheets), ("sdxl", false));
        assert!(c.messages.iter().any(|m| matches!(m, Msg::Note { text, .. } if text.contains("turnaround"))));
    }

    /// The board of `film_chat` drawn whole, on Wan or H3.
    fn ready(h3: bool) -> Chat {
        let mut c = film_chat();
        let t = film_tk(&c, h3);
        story(&mut c, &t, board());
        board_advance(&mut c, 5.0, true);
        while let Step::CastPicture(k) | Step::CastSheet(k) | Step::Keyframe(k) = next_step(&c) {
            let step = next_step(&c);
            board_picture_done(&mut c, step, img(&format!("p{k}")), format!("p{k}-s0"), 5.0);
        }
        assert_eq!(c.film.as_ref().unwrap().phase, Phase::Ready);
        c
    }

    fn take_reply(c: &mut Chat, answers: Value, better: &str) {
        let t = tk(c);
        apply_reply(c, Mode::Take, reply(json!({"notes": "", "answers": answers, "better_text": better, "reply": ""})), &t, &Locks::default(), vec![], Default::default());
    }

    #[test]
    fn a_wan_film_cuts_from_its_keyframes_and_an_h3_one_from_its_cast() {
        let c = ready(false);
        let f = c.film.as_ref().unwrap();
        let s1 = shot_spec(f, 1, 7);
        assert!(!s1.cut && s1.keyframe.is_none() && s1.references.is_empty(), "a continue starts from the last frame");
        let s2 = shot_spec(f, 2, 7);
        assert!(s2.cut && s2.keyframe.as_deref() == Some("p2-s0") && s2.references.is_empty());
        assert!(shot_spec(f, 0, 7).keyframe.is_none(), "shot 1's keyframe is the clip's source");
        let c = ready(true);
        let f = c.film.as_ref().unwrap();
        assert_eq!(shot_spec(f, 0, 7).references, ["mara-photo"], "shot 1 follows its cast");
        let s2 = shot_spec(f, 2, 7);
        assert!(s2.cut && s2.keyframe.is_none() && s2.references == ["p1-s0"]);
    }

    #[test]
    fn each_shot_is_looked_at_and_a_failed_one_is_filmed_once_more() {
        let mut c = ready(false);
        assert!(can_go(c.film.as_ref().unwrap()));
        go(&mut c, true).unwrap();
        assert_eq!(next_step(&c), Step::Shot(0));
        shot_done(&mut c, 0, 11, "clip1");
        assert_eq!(next_step(&c), Step::Turn(Mode::Take));
        assert_eq!(take_checks(c.film.as_ref().unwrap(), 0).len(), 2, "the words, and Mara's likeness");
        take_reply(&mut c, json!([true, true]), "");
        assert_eq!(next_step(&c), Step::Shot(1));
        shot_done(&mut c, 1, 12, "clip1");
        assert_eq!(take_checks(c.film.as_ref().unwrap(), 1).len(), 3, "a continue is checked for carrying on");
        take_reply(&mut c, json!([false, true, true]), "She steps out into **heavy** rain.");
        assert_eq!(next_step(&c), Step::Shot(1), "a retake");
        assert_eq!(c.film.as_ref().unwrap().shots[1].text, "She steps out into heavy rain.");
        shot_done(&mut c, 1, 13, "clip1");
        take_reply(&mut c, json!([false]), "");
        assert_eq!(next_step(&c), Step::Shot(2), "once only: it goes on");
        shot_done(&mut c, 2, 14, "clip1");
        take_reply(&mut c, json!([true, true]), "");
        assert_eq!(next_step(&c), Step::End);
        let f = c.film.as_ref().unwrap();
        assert_eq!(f.phase, Phase::Done);
        assert_eq!(f.shots[1].takes, 2);
        assert!(c.messages.iter().any(|m| matches!(m, Msg::Clip { id, .. } if id == "clip1")));
        assert!(matches!(c.messages.last(), Some(Msg::Note { text, .. }) if text.contains("shot 2 (2 takes)")));
        assert!(go(&mut c, true).is_err(), "a film is shot once");
    }

    #[test]
    fn an_agent_that_cant_see_films_every_shot_and_says_none_was_checked() {
        let mut c = ready(true);
        go(&mut c, false).unwrap();
        for k in 0..3 {
            assert_eq!(next_step(&c), Step::Shot(k));
            shot_done(&mut c, k, 1, "clip1");
        }
        assert_eq!(next_step(&c), Step::End);
        assert!(matches!(c.messages.last(), Some(Msg::Note { text, .. }) if text.contains("No shot was checked")));
    }

    #[test]
    fn a_stopped_film_goes_on_from_its_shot() {
        let mut c = ready(false);
        go(&mut c, true).unwrap();
        shot_done(&mut c, 0, 11, "clip1");
        take_reply(&mut c, json!([true, true]), "");
        c.session.next = Next::Nothing; // stopped
        assert!(can_go(c.film.as_ref().unwrap()));
        go(&mut c, true).unwrap();
        assert_eq!(next_step(&c), Step::Shot(1));
        assert_eq!(c.film.as_ref().unwrap().clip.as_deref(), Some("clip1"));
    }

    #[test]
    fn a_rewritten_board_keeps_the_pictures_that_still_fit() {
        let mut c = film_chat();
        let t = film_tk(&c, false);
        story(&mut c, &t, board());
        board_advance(&mut c, 5.0, true);
        board_picture_done(&mut c, Step::CastPicture(1), img("leo"), "leo-s0".into(), 5.0);
        board_picture_done(&mut c, Step::Keyframe(0), img("k0"), "k0-s0".into(), 5.0);
        board_picture_done(&mut c, Step::Keyframe(2), img("k2"), "k2-s0".into(), 5.0);
        c.messages.push(Msg::User { n: 9, ms: 0, text: "Make shot 3 at night, and Mara in blue.".into(), images: vec![] });
        let mut b = board();
        b["cast"][0]["look"] = json!("a tall woman in a blue coat");
        b["cast"][0]["picture"] = json!(0);
        b["shots"][2]["text"] = json!("Leo waits at the bus stop at night.");
        story(&mut c, &t, b.clone());
        let f = c.film.as_ref().unwrap();
        assert_eq!(f.cast[0].stem.as_deref(), Some("mara-photo"), "the person's picture stays, whatever the look says");
        assert_eq!(f.cast[1].stem.as_deref(), Some("leo-s0"), "same name, same look");
        assert_eq!(f.shots[0].key_stem.as_deref(), Some("k0-s0"), "an unchanged shot keeps its keyframe");
        assert!(f.shots[2].key_stem.is_none(), "a reworded one draws its own");
        assert_eq!(f.phase, Phase::Board, "back to drawing, when the person says");
        board_advance(&mut c, 5.0, true);
        assert_eq!(next_step(&c), Step::Keyframe(2));
        // A drawn picture whose look changed is drawn again.
        b["cast"][1]["look"] = json!("a boy in a **green** raincoat");
        story(&mut c, &t, b);
        assert_eq!(c.film.as_ref().unwrap().cast[1].look, "a boy in a green raincoat", "no Markdown in a look");
        board_advance(&mut c, 5.0, true);
        assert_eq!(next_step(&c), Step::CastPicture(1));
    }

    #[test]
    fn a_session_runs_plan_rounds_and_critiques_then_stops_at_its_limit() {
        let mut c = chat();
        assert_eq!(next_step(&c), Step::Turn(Mode::Plan));
        let t = tk(&c);
        apply_reply(&mut c, Mode::Plan, reply(json!({"notes": "", "checks": ["Is there a hat?", "Is it red?"], "rounds": 2, "count": 2, "reply": "ok"})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.checks.len(), 2);
        assert_eq!(c.session.next, Next::Batch);
        start_round(&mut c, 100);
        assert_eq!(next_step(&c), Step::Image { round: 1, k: 0 });
        assert_eq!(seed_for(&c, 1), 101, "the seed is locked by default");
        image_done(&mut c, img("a"));
        assert_eq!(next_step(&c), Step::Image { round: 1, k: 1 });
        image_done(&mut c, img("b"));
        assert_eq!(next_step(&c), Step::Turn(Mode::Critique));
        let r = reply(json!({"notes": "", "checks": [[true, false], [true, true]], "keepers": [2, 9], "action": "generate", "reply": "",
                             "change": {"prompt": "a fox in a bright red hat"}}));
        apply_reply(&mut c, Mode::Critique, r, &t, &Locks::default(), vec![], Default::default());
        let Some(Msg::Agent { passed, keepers, .. }) = c.messages.last().cloned() else { panic!() };
        assert_eq!(passed, [2]);
        assert_eq!(keepers, [2], "a keeper outside the batch is dropped");
        assert_eq!(c.settings["prompt"], json!("a fox in a bright red hat"));
        assert_eq!(c.session.next, Next::Batch);
        start_round(&mut c, 999);
        assert_eq!(seed_for(&c, 0), 100, "the same seeds every round");
        image_done(&mut c, img("c"));
        image_done(&mut c, img("d"));
        let r = reply(json!({"notes": "", "checks": [[true, true], [true, true]], "keepers": [1], "action": "generate", "reply": ""}));
        apply_reply(&mut c, Mode::Critique, r, &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.next, Next::Nothing, "round 2 of 2 was the last");
        assert_eq!(next_step(&c), Step::End);
    }

    #[test]
    fn a_chat_draws_as_many_as_asked_and_keeps_seeds_only_when_asked() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let draw = |count: u32, seeds: &str| reply(json!({"notes": "", "action": "generate", "count": count, "seeds": seeds,
                                                       "change": {"prompt": "a cat"}, "reason": "", "reply": "a cat"}));
        apply_reply(&mut c, Mode::Chat, draw(1, "new"), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 100);
        assert_eq!(round_size(&c), 1, "one asked for is one drawn");
        image_done(&mut c, img("a"));
        assert_eq!(next_step(&c), Step::End);
        apply_reply(&mut c, Mode::Chat, draw(99, "same"), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 200);
        assert_eq!((round_size(&c), seed_for(&c, 0)), (MAX_BATCH, 100), "clamped to the cap; the same seeds");
        for k in ["b", "c", "d", "e", "e2", "e3", "e4", "e5"] {
            image_done(&mut c, img(k));
        }
        apply_reply(&mut c, Mode::Chat, draw(2, "new"), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 300);
        assert_eq!((c.session.round, seed_for(&c, 0)), (3, 300), "rounds count on; new seeds");
        let unsaid = reply(json!({"notes": "", "action": "generate", "change": {"prompt": "a cat"}, "reply": ""}));
        for k in ["f", "g"] {
            image_done(&mut c, img(k));
        }
        apply_reply(&mut c, Mode::Chat, unsaid, &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 400);
        assert_eq!(round_size(&c), 1, "one picture when no number was said");
    }

    #[test]
    fn a_chat_reply_starts_a_session_on_its_own_goal_and_the_plan_turn_follows() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.checks = vec!["an old check".into()];
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let start = |goal: &str| reply(json!({"notes": "", "action": "session", "goal": goal, "reply": "On it."}));
        apply_reply(&mut c, Mode::Chat, start("  "), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(next_step(&c), Step::End, "no goal, no session");
        assert!(matches!(c.messages.last(), Some(Msg::Note { .. })));
        apply_reply(&mut c, Mode::Chat, start("A fox wearing a red hat"), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.goal, "A fox wearing a red hat");
        assert!(c.session.checks.is_empty(), "the plan writes its own");
        assert_eq!(next_step(&c), Step::Turn(Mode::Plan));
    }

    #[test]
    fn a_comparison_draws_a_round_per_value_on_the_same_seeds_then_counts_and_picks() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let go = |c: &mut Chat, mode: Mode, v: Value| apply_reply(c, mode, reply(v), &t, &Locks::default(), vec![], Default::default());
        go(&mut c, Mode::Chat, json!({"notes": "a comparison", "action": "compare", "goal": "Which CFG keeps the hat red?",
                                      "vary": "cfg_scale", "values": ["4", "9", "4", "x"], "count": 2, "reply": "Let's see."}));
        let sw = c.session.sweep.clone().expect("a comparison");
        assert_eq!(sw.values, [json!(4.0), json!(9.0)], "read as numbers, once each; the unreadable dropped");
        assert_eq!(sw.before, json!(6.0));
        assert_eq!(next_step(&c), Step::Turn(Mode::Plan));
        go(&mut c, Mode::Plan, json!({"notes": "", "checks": ["Is the hat red?", "Is it a fox?"], "reply": ""}));
        assert_eq!((c.session.rounds, round_size(&c)), (2, 2));
        for (i, (seed, answers)) in [(100, json!([[true, true], [true, true]])), (555, json!([[false, true], [true, true]]))].into_iter().enumerate() {
            assert!(needs_round(&c));
            start_round(&mut c, seed);
            assert_eq!(c.settings["cfg_scale"], sw.values[i], "round {i} draws its value");
            assert_eq!(seed_for(&c, 0), 100, "the same seeds every round");
            let Some(Msg::Batch { summary, .. }) = c.messages.last() else { panic!("a round") };
            assert!(summary.starts_with(&format!("CFG scale {} · ", [4, 9][i])), "{summary}");
            image_done(&mut c, img("a"));
            image_done(&mut c, img("b"));
            assert_eq!(next_step(&c), Step::Turn(Mode::Critique));
            go(&mut c, Mode::Critique, json!({"notes": "", "checks": answers, "keepers": [], "reply": ""}));
        }
        assert_eq!(next_step(&c), Step::Turn(Mode::Verdict));
        let Some(Msg::Note { text, .. }) = c.messages.last().cloned() else { panic!("the counts are a note") };
        assert!(text.contains("1. CFG scale 4: 2 of 2 passed every check (1 2/2, 2 2/2)"), "{text}");
        assert!(text.contains("2. CFG scale 9: 1 of 2 passed every check (1 1/2, 2 2/2)"), "{text}");
        go(&mut c, Mode::Verdict, json!({"notes": "", "best": 1, "reply": "CFG 4: every picture passed."}));
        assert_eq!(c.settings["cfg_scale"], json!(4.0));
        assert_eq!(next_step(&c), Step::End);
        let change = c.changes.last().unwrap().id;
        undo(&mut c, change).unwrap();
        assert_eq!(c.settings["cfg_scale"], json!(6.0), "Undo returns to before the comparison");
    }

    #[test]
    fn a_comparison_of_a_locked_setting_or_one_value_draws_nothing() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let ask = json!({"notes": "", "action": "compare", "goal": "best CFG", "vary": "cfg_scale", "values": ["3", "7"], "count": 2, "reply": ""});
        apply_reply(&mut c, Mode::Chat, reply(ask), &t, &Locks::of(true, false, None), vec![], Default::default());
        assert_eq!(next_step(&c), Step::End);
        assert!(c.session.sweep.is_none() && c.session.goal.is_empty());
        let notes: Vec<String> = c.messages.iter().filter_map(|m| match m { Msg::Note { text, .. } => Some(text.clone()), _ => None }).collect();
        assert!(notes.iter().any(|n| n.contains("locked")) && notes.iter().any(|n| n.contains("needs two values")), "{notes:?}");
    }

    #[test]
    fn a_comparison_may_switch_to_a_model_that_takes_what_this_one_locks() {
        let mut c = chat();
        c.settings = json!({"prompt": "a cat", "template": "klein"});
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let mut t = build(&Rules::default(), &c.settings, &Locks::of(true, false, None), vec![
            ModelChoice { name: "klein".into(), family: "flux2-klein-4b", label: "FLUX.2 Klein 4B", references: false, locks: Locks::of(true, false, None) },
            ModelChoice { name: "moss".into(), family: "moss_image", label: "MOSS-Image", references: false, locks: Locks::default() },
        ], Some(ImageFamily::Sdxl));
        t.locks = Locks::of(true, false, None);
        let ask = |model: &str| reply(json!({"notes": "", "action": "compare", "change": {"model": model}, "reason": "",
            "goal": "best CFG", "vary": "cfg_scale", "values": ["3", "5", "7"], "count": 2, "reply": ""}));
        apply_reply(&mut c, Mode::Chat, ask("moss"), &t, &t.locks.clone(), vec![], Default::default());
        assert_eq!(c.settings["template"], json!("moss"));
        assert_eq!(c.session.sweep.as_ref().map(|s| s.values.len()), Some(3), "{:?}", c.messages);
        assert_eq!(next_step(&c), Step::Turn(Mode::Plan));
        // Staying on a model without CFG, there is nothing to compare.
        let mut c2 = chat();
        c2.settings = json!({"prompt": "a cat", "template": "klein"});
        c2.session.goal.clear();
        c2.session.mode = Mode::Chat;
        apply_reply(&mut c2, Mode::Chat, ask("klein"), &t, &t.locks.clone(), vec![], Default::default());
        assert!(c2.session.sweep.is_none());
    }

    #[test]
    fn a_verdict_that_picks_nothing_puts_the_setting_back() {
        let mut c = chat();
        c.session.sweep = Some(Sweep { knob: "cfg_scale".into(), values: vec![json!(3.0), json!(9.0)], before: json!(6.0) });
        c.settings["cfg_scale"] = json!(9.0);
        let t = tk(&c);
        apply_reply(&mut c, Mode::Verdict, reply(json!({"notes": "", "reply": "Hard to say."})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.settings["cfg_scale"], json!(6.0));
    }

    #[test]
    fn a_session_is_judged_against_the_pictures_its_message_attached() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        c.messages.push(Msg::User { n: c.next_n(), ms: 0, text: "find the best prompt for this".into(), images: vec![img("ref")] });
        let t = tk(&c);
        apply_reply(&mut c, Mode::Chat, reply(json!({"notes": "", "action": "improve", "goal": "Match the picture", "reply": ""})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.reference, [img("ref")]);
        assert_eq!(next_step(&c), Step::Turn(Mode::Plan));
    }

    #[test]
    fn a_chat_reply_describing_a_picture_keeps_the_prompt_and_ends() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let r = reply(json!({"notes": "", "action": "describe", "prompt": "a red fox ", "negative": "blurry", "reply": "Here it is."}));
        apply_reply(&mut c, Mode::Chat, r, &t, &Locks::default(), vec![], Default::default());
        let Some(Msg::Agent { prompt, negative, .. }) = c.messages.last().cloned() else { panic!() };
        assert_eq!((prompt.as_deref(), negative.as_deref()), (Some("a red fox"), Some("blurry")));
        assert_eq!(next_step(&c), Step::End);
    }

    #[test]
    fn a_round_is_explained_by_the_change_that_led_to_it_else_the_reply() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        let said = |reason: &str, prompt: &str| reply(json!({"notes": "", "action": "generate", "count": 1, "seeds": "new",
            "change": {"prompt": prompt}, "reason": reason, "reply": "Here is a cat."}));
        apply_reply(&mut c, Mode::Chat, said("black fur, as asked", "a black cat"), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 100);
        image_done(&mut c, img("a"));
        assert_eq!(round_reason(&c, 1), "black fur, as asked");
        // The same prompt again is no change, so the reply is the explanation.
        apply_reply(&mut c, Mode::Chat, said("", "a black cat"), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 200);
        assert_eq!(round_reason(&c, 2), "Here is a cat.");
        assert_eq!(round_reason(&c, 9), "", "no such round");
    }

    #[test]
    fn a_session_after_a_round_judges_that_round_first() {
        let mut c = chat();
        c.session.goal.clear();
        c.session.mode = Mode::Chat;
        start_round(&mut c, 100);
        image_done(&mut c, img("a"));
        image_done(&mut c, img("b"));
        // As '/agent/send' begins a session.
        c.session.goal = "pick the best fox".into();
        c.session.mode = Mode::Plan;
        c.session.first_round = c.session.round;
        let t = tk(&c);
        apply_reply(&mut c, Mode::Plan, reply(json!({"notes": "", "checks": ["Is it a fox?"], "change": {}, "reason": "", "reply": ""})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(next_step(&c), Step::Turn(Mode::Critique), "the round on screen is judged, not redrawn");
        assert_eq!(session_rounds(&c), 1, "and counts as the session's first");
        let r = reply(json!({"notes": "", "checks": [[true], [false]], "keepers": [1], "action": "generate", "change": {}, "reason": "", "reply": ""}));
        apply_reply(&mut c, Mode::Critique, r, &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 999);
        assert_eq!((c.session.round, seed_for(&c, 0)), (2, 100), "round 2, the judged round's seeds");

        // A plan that rewrites the prompt draws afresh instead.
        let mut c = chat();
        c.session.mode = Mode::Chat;
        start_round(&mut c, 100);
        image_done(&mut c, img("a"));
        image_done(&mut c, img("b"));
        c.session.mode = Mode::Plan;
        c.session.first_round = c.session.round;
        apply_reply(&mut c, Mode::Plan, reply(json!({"notes": "", "checks": ["Is it a dog?"], "change": {"prompt": "a dog"}, "reason": "", "reply": ""})), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.next, Next::Batch);
        start_round(&mut c, 500);
        assert_eq!(seed_for(&c, 0), 500);
    }

    #[test]
    fn a_plan_sizes_its_session_inside_the_caps() {
        let mut c = chat();
        c.session.rounds = 0;
        c.session.per_round = 0;
        let t = tk(&c);
        let plan = |rounds: u32, count: u32| reply(json!({"notes": "", "checks": ["Is it a fox?"], "rounds": rounds, "count": count,
                                                        "change": {"prompt": "a fox"}, "reason": "", "reply": ""}));
        apply_reply(&mut c, Mode::Plan, plan(2, 3), &t, &Locks::default(), vec![], Default::default());
        start_round(&mut c, 100);
        assert_eq!((session_limit(&c), round_size(&c)), (2, 3), "two rounds of three, as asked");
        apply_reply(&mut c, Mode::Plan, plan(50, 50), &t, &Locks::default(), vec![], Default::default());
        assert_eq!((session_limit(&c), per_round(&c)), (MAX_ROUNDS, MAX_BATCH), "held to the caps");
        let unsaid = reply(json!({"notes": "", "checks": ["Is it a fox?"], "change": {"prompt": "a fox"}, "reason": "", "reply": ""}));
        apply_reply(&mut c, Mode::Plan, unsaid, &t, &Locks::default(), vec![], Default::default());
        assert_eq!((session_limit(&c), per_round(&c)), (SESSION_ROUNDS, SESSION_BATCH));
    }

    #[test]
    fn an_escalation_needs_its_pass_switched_on() {
        let mut c = chat();
        start_round(&mut c, 1);
        image_done(&mut c, img("a"));
        image_done(&mut c, img("b"));
        let r = || reply(json!({"notes": "", "checks": [[true],[true]], "keepers": [1], "action": "escalate", "escalate": {"image": 1, "pass": "hires"}, "reply": ""}));
        let t = tk(&c);
        apply_reply(&mut c, Mode::Critique, r(), &t, &Locks::default(), vec![], Default::default());
        assert_eq!(c.session.next, Next::Nothing, "hires is off by default");
        assert!(c.actions.is_empty());
        let mut rules = Rules::default();
        rules.actions.insert("hires".into());
        let t = tk_under(&c, &rules);
        apply_reply(&mut c, Mode::Critique, r(), &t, &Locks::default(), vec![], Default::default());
        let id = c.actions[0].id;
        assert_eq!(c.actions[0].item, "a");
        assert_eq!(next_step(&c), Step::Escalate(id));
    }

    #[test]
    fn undo_puts_back_only_what_is_still_as_the_change_left_it() {
        let mut c = chat();
        c.session.mode = Mode::Chat;
        let t = tk(&c);
        apply_reply(&mut c, Mode::Chat, reply(json!({"notes": "", "reply": "", "action": "none", "change": {"cfg_scale": 7.0}})), &t, &Locks::default(), vec![], Default::default());
        let id = c.changes[0].id;
        let undo_id = undo(&mut c, id).unwrap();
        assert_eq!(c.settings["cfg_scale"], json!(6.0));
        // Undoing the undo redoes.
        undo(&mut c, undo_id).unwrap();
        assert_eq!(c.settings["cfg_scale"], json!(7.0));
        // Changed since by hand: refused.
        let last = c.changes.last().unwrap().id;
        c.settings["cfg_scale"] = json!(3.0);
        assert!(undo(&mut c, last).is_err());
    }

    #[test]
    fn a_reply_in_a_code_fence_still_reads() {
        assert_eq!(parse_reply("```json\n{\"notes\":\"a\",\"reply\":\"b\"}\n```").unwrap().reply, "b");
        assert!(parse_reply("no json here").is_err());
    }
}
