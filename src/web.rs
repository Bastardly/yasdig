//! The static files the page is made of, and the one route that serves them.

use crate::http;
use anyhow::Result;
use std::path::Path;

/// Every file the browser fetches, by exact name, with what to answer it with.
///
/// A table rather than a match arm each, because there are twenty of them now —
/// but it is still a whitelist. A request names something that is in this list or
/// gets a 404: the name is compared whole, never joined from what was asked for
/// beyond this, so there is nothing to traverse out of. Same rule as
/// `workitems::valid_id`, and the reason there is still no generic static handler.
///
/// `doctor` reads the same table (see `preflight`), so a module added here is a
/// module `doctor` checks for.
///
/// **Each row carries its bytes**, so a release is one file with the page
/// inside it. They are read from disk instead whenever there is a `web/` beside
/// `models.toml` — see [`serve`].
///
/// The content type is load-bearing for the `js/` rows in a way it wasn't for the
/// stylesheet: a browser refuses to execute a `type="module"` script served as
/// anything but a JavaScript MIME type, and the failure is a console error with a
/// blank page behind it.
pub struct WebFile {
    pub name: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
}

/// One row: the name, its type, and the file itself, embedded at build time.
/// A macro because `include_bytes!` needs a literal, and spelling the name
/// twice per row is how the two would come to disagree.
macro_rules! row {
    ($name:literal, $ty:literal) => {
        WebFile {
            name: $name,
            content_type: $ty,
            bytes: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/web/", $name)),
        }
    };
}

pub const WEB_FILES: &[WebFile] = &[
    row!("index.html", "text/html; charset=utf-8"),
    row!("style.css", "text/css; charset=utf-8"),
    row!("js/app.js", "text/javascript; charset=utf-8"),
    row!("js/busy.js", "text/javascript; charset=utf-8"),
    row!("js/convert.js", "text/javascript; charset=utf-8"),
    row!("js/dom.js", "text/javascript; charset=utf-8"),
    row!("js/getmodels.js", "text/javascript; charset=utf-8"),
    row!("js/gridmark.js", "text/javascript; charset=utf-8"),
    row!("js/hints.js", "text/javascript; charset=utf-8"),
    row!("js/image.js", "text/javascript; charset=utf-8"),
    row!("js/jobs.js", "text/javascript; charset=utf-8"),
    row!("js/help.js", "text/javascript; charset=utf-8"),
    row!("js/loras.js", "text/javascript; charset=utf-8"),
    row!("js/memory.js", "text/javascript; charset=utf-8"),
    row!("js/redraw.js", "text/javascript; charset=utf-8"),
    row!("js/refs.js", "text/javascript; charset=utf-8"),
    row!("js/modelsettings.js", "text/javascript; charset=utf-8"),
    row!("js/outputs.js", "text/javascript; charset=utf-8"),
    row!("js/player.js", "text/javascript; charset=utf-8"),
    row!("js/prompt-box.js", "text/javascript; charset=utf-8"),
    row!("js/prompt-weight.js", "text/javascript; charset=utf-8"),
    row!("js/settings.js", "text/javascript; charset=utf-8"),
    row!("js/source-slot.js", "text/javascript; charset=utf-8"),
    row!("js/state.js", "text/javascript; charset=utf-8"),
    row!("js/video.js", "text/javascript; charset=utf-8"),
    row!("js/viewer.js", "text/javascript; charset=utf-8"),
    row!("js/regiontool.js", "text/javascript; charset=utf-8"),
    row!("js/layout.js", "text/javascript; charset=utf-8"),
    row!("js/clipview.js", "text/javascript; charset=utf-8"),
    row!("js/brushpaint.js", "text/javascript; charset=utf-8"),
    row!("js/colorrange.js", "text/javascript; charset=utf-8"),
    row!("js/colorwheel.js", "text/javascript; charset=utf-8"),
    row!("js/panels.js", "text/javascript; charset=utf-8"),
    row!("js/parts.js", "text/javascript; charset=utf-8"),
    row!("js/mirror.js", "text/javascript; charset=utf-8"),
    row!("js/texttool.js", "text/javascript; charset=utf-8"),
    row!("js/workitems.js", "text/javascript; charset=utf-8"),
];

/// The file a GET path asks for, or `None` if it isn't one of ours. `/` is the
/// page, which is the only name the client ever has to know.
pub fn lookup(path: &str) -> Option<&'static WebFile> {
    let name = match path {
        "/" => "index.html",
        p => p.strip_prefix('/')?,
    };
    WEB_FILES.iter().find(|f| f.name == name)
}

/// From disk when there is a `web/` to read — a source checkout, where reading
/// fresh per request is what lets an edit show on reload with no restart — and
/// otherwise the copy built in. A release's folder never gets a `web/`, so the
/// built-in copy can't be shadowed by a stale one there.
///
/// Answered `no-cache` either way: without a validator a browser may cache
/// heuristically, and an edited stylesheet or module that doesn't appear on
/// reload is a trap that costs an hour to find. After an upgrade the same rule
/// is what makes the new page load instead of the old one.
pub fn serve(conn: &mut http::Connection, web_dir: Option<&Path>, file: &WebFile) -> Result<()> {
    let body = match web_dir {
        Some(dir) => match std::fs::read(dir.join(file.name)) {
            Ok(b) => std::borrow::Cow::Owned(b),
            Err(_) => {
                let msg = format!("web/{} not found", file.name);
                return conn.respond(500, "text/plain", msg.as_bytes());
            }
        },
        None => std::borrow::Cow::Borrowed(file.bytes),
    };
    conn.respond_with(200, file.content_type, "Cache-Control: no-cache
", &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whitelist is the whole traversal guard on this route, so the two
    /// things it must do are: answer the names in the table, and answer nothing
    /// else. A `..` that resolved would reach the checkpoints directory.
    #[test]
    fn only_the_listed_names_resolve() {
        assert_eq!(lookup("/").unwrap().name, "index.html");
        assert_eq!(lookup("/js/app.js").unwrap().name, "js/app.js");
        for bad in [
            "/../models.toml",
            "/js/../../models.toml",
            "/js/",
            "/js/nope.js",
            "//style.css",
            "/style.css/",
            "/STYLE.CSS",
        ] {
            assert!(lookup(bad).is_none(), "{bad} should not resolve");
        }
    }

    /// Every module `app.js` can reach has to be in the table, or the page dies
    /// on a 404 that only shows up in the console. Read off the directory rather
    /// than listed again here, because a second list is a second thing to forget.
    #[test]
    fn the_table_lists_every_file_under_web() {
        let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
        let mut found: Vec<String> = vec![];
        for entry in std::fs::read_dir(&web).unwrap() {
            let p = entry.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            if p.is_dir() {
                for sub in std::fs::read_dir(&p).unwrap() {
                    let sub = sub.unwrap().path();
                    let leaf = sub.file_name().unwrap().to_string_lossy().into_owned();
                    // `*.test.js` runs under `node --test` and must never be
                    // served — it is not part of the page, and a whitelist that
                    // listed it would publish the test suite to the browser.
                    // Skipped here rather than added to the table, so the check
                    // this test exists for still fires for every real module.
                    if leaf.ends_with(".test.js") {
                        continue;
                    }
                    found.push(format!("{name}/{leaf}"));
                }
            } else {
                found.push(name);
            }
        }
        found.sort();
        let mut listed: Vec<String> = WEB_FILES.iter().map(|f| f.name.to_string()).collect();
        listed.sort();
        listed.dedup();
        assert_eq!(listed, found);
    }
}
