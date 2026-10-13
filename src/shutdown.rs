//! What *Close server* clears on the way out (`POST /shutdown`).
//!
//! Three switches in `user/settings.toml`, all off by default, because the
//! one-file release has no console to close and `work/` grows with every
//! generation: a person who never looks inside that folder should still be
//! able to keep it from filling the disk.
//!
//! **Only what the app itself lists is removed**, through the deletes that
//! already exist — `Store::delete`, `Store::delete_output`, `VideoStore::delete`.
//! Not `remove_dir_all` on a directory: those routines carry the `valid_id`
//! whitelist and know which files belong to an entry, and a wholesale wipe of a
//! folder the user can also reach in a file manager would take whatever they
//! put there with it. The favourite directories are never named here at all,
//! which is the whole point of keeping one.
//!
//! Best effort: a file that won't go is logged and the close happens anyway.
//! Refusing to exit over a locked thumbnail would leave the person with a
//! server they can't stop from the page — the problem this exists to solve.

use crate::config::Config;
use crate::settings::Settings;
use crate::videos::VideoStore;
use crate::workitems::{Gallery, Shelf, Store};

/// How many entries each switch removed, for the answer and the log.
#[derive(Debug, Default, serde::Serialize)]
pub struct Cleared {
    pub workitems: usize,
    pub outputs: usize,
    pub videos: usize,
}

pub fn clear(cfg: &Config) -> Cleared {
    let settings = Settings::load_or_default(&cfg.user_dir());
    let work = cfg.base_dir.join("work");
    clear_in(&Store::new(&work, &cfg.user_dir()), &VideoStore::new(&work), &settings)
}

fn clear_in(store: &Store, videos: &VideoStore, s: &Settings) -> Cleared {
    let mut out = Cleared::default();
    let log = |e: anyhow::Error| eprintln!("[yasdig] clearing on close: {e:#}");
    if s.clear_workitems_on_close {
        for it in store.list(Shelf::Work) {
            match store.delete(Shelf::Work, &it.id) {
                Ok(()) => out.workitems += 1,
                Err(e) => log(e),
            }
        }
    }
    if s.clear_outputs_on_close {
        for o in store.list_outputs(Gallery::Output) {
            match store.delete_output(Gallery::Output, &o.stem) {
                Ok(()) => out.outputs += 1,
                Err(e) => log(e),
            }
        }
    }
    if s.clear_videos_on_close {
        for c in videos.list() {
            match videos.delete(&c.id) {
                Ok(()) => out.videos += 1,
                Err(e) => log(e),
            }
        }
        // A clip's enlarged frames: `Engine::enlarge_clip` writes them to
        // `work/output/{id}-x{factor}/`, the only directories anything puts
        // there. All of them rather than only the listed clips', since an
        // enlargement outlives a clip deleted from the grid and would
        // otherwise never go. Matched on the name it is given, not on
        // "is a directory", so a folder the user made is left alone.
        if let Ok(entries) = std::fs::read_dir(store.output_dir()) {
            for e in entries.flatten() {
                let path = e.path();
                let name = e.file_name();
                let Some(name) = name.to_str() else { continue };
                if path.is_dir() && is_enlarged_clip(name) {
                    if let Err(err) = std::fs::remove_dir_all(&path) {
                        log(anyhow::Error::new(err).context(format!("deleting {}", path.display())));
                    }
                }
            }
        }
    }
    out
}

/// `{clip id}-x{percent}`, as `enlarge_clip` names it. A clip id is
/// `new_id`'s `{ms}-{seed}`, digits both sides — matched that tightly because
/// `valid_id` alone also takes `holiday-x2`, a folder somebody made.
fn is_enlarged_clip(name: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    name.rsplit_once("-x").is_some_and(|(id, pct)| {
        digits(pct) && id.split_once('-').is_some_and(|(ms, seed)| digits(ms) && digits(seed))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_ticked_galleries_go_and_favourites_never_do() {
        let dir = std::env::temp_dir().join(format!(
            "yasdig-close-{}",
            crate::workitems::new_id(5)
        ));
        let work = dir.join("work");
        let store = Store::new(&work, &dir.join("user"));
        let videos = VideoStore::new(&work);
        let png = dir.join("src.png");
        image::RgbaImage::from_pixel(32, 16, image::Rgba([7, 7, 7, 255]))
            .save(&png)
            .unwrap();
        let stem = store.import(&std::fs::read(&png).unwrap(), 32, 16).unwrap();
        store.favorite_output(&stem).unwrap();
        let enlarged = store.output_dir().join("123-4-x200");
        let mine = store.output_dir().join("my stuff");
        std::fs::create_dir_all(&enlarged).unwrap();
        std::fs::create_dir_all(&mine).unwrap();

        // Nothing ticked: nothing goes.
        let none = clear_in(&store, &videos, &Settings::default());
        assert_eq!(none.outputs, 0);
        assert_eq!(store.list_outputs(Gallery::Output).len(), 1);

        let s = Settings {
            clear_outputs_on_close: true,
            clear_videos_on_close: true,
            ..Settings::default()
        };
        let got = clear_in(&store, &videos, &s);
        assert_eq!(got.outputs, 1);
        assert!(store.list_outputs(Gallery::Output).is_empty());
        assert_eq!(store.list_outputs(Gallery::Favorites).len(), 1);
        assert!(!enlarged.exists());
        assert!(mine.is_dir(), "a folder the app didn't name must stay");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_enlarged_clip_is_recognised_by_its_name() {
        assert!(is_enlarged_clip("1758104523000-12345-x200"));
        assert!(!is_enlarged_clip("my stuff"));
        assert!(!is_enlarged_clip("123-x"));
        assert!(!is_enlarged_clip("123-xabc"));
        assert!(!is_enlarged_clip("holiday-x2"), "a folder the user made");
        assert!(!is_enlarged_clip("my-holiday-x2"));
    }
}
