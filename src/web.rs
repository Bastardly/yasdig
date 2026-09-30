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
/// The content type is load-bearing for the `js/` rows in a way it wasn't for the
/// stylesheet: a browser refuses to execute a `type="module"` script served as
/// anything but a JavaScript MIME type, and the failure is a console error with a
/// blank page behind it.
pub const WEB_FILES: &[(&str, &str)] = &[
    ("index.html", "text/html; charset=utf-8"),
    ("style.css", "text/css; charset=utf-8"),
    ("js/app.js", "text/javascript; charset=utf-8"),
    ("js/busy.js", "text/javascript; charset=utf-8"),
    ("js/convert.js", "text/javascript; charset=utf-8"),
    ("js/dom.js", "text/javascript; charset=utf-8"),
    ("js/gridmark.js", "text/javascript; charset=utf-8"),
    ("js/hints.js", "text/javascript; charset=utf-8"),
    ("js/image.js", "text/javascript; charset=utf-8"),
    ("js/img2img.js", "text/javascript; charset=utf-8"),
    ("js/loras.js", "text/javascript; charset=utf-8"),
    ("js/modelsettings.js", "text/javascript; charset=utf-8"),
    ("js/outputs.js", "text/javascript; charset=utf-8"),
    ("js/player.js", "text/javascript; charset=utf-8"),
    ("js/prompt-box.js", "text/javascript; charset=utf-8"),
    ("js/prompt-weight.js", "text/javascript; charset=utf-8"),
    ("js/settings.js", "text/javascript; charset=utf-8"),
    ("js/state.js", "text/javascript; charset=utf-8"),
    ("js/video.js", "text/javascript; charset=utf-8"),
    ("js/viewer.js", "text/javascript; charset=utf-8"),
    ("js/workitems.js", "text/javascript; charset=utf-8"),
];

/// The file a GET path asks for, or `None` if it isn't one of ours. `/` is the
/// page, which is the only name the client ever has to know.
pub fn lookup(path: &str) -> Option<(&'static str, &'static str)> {
    let name = match path {
        "/" => "index.html",
        p => p.strip_prefix('/')?,
    };
    WEB_FILES.iter().copied().find(|(n, _)| *n == name)
}

/// Read fresh per request so an edit needs no restart — and answered `no-cache`,
/// because without a validator a browser may cache heuristically, and an edited
/// stylesheet or module that doesn't appear on reload is a trap that costs an
/// hour to find.
pub fn serve(
    conn: &mut http::Connection,
    web_dir: &Path,
    name: &str,
    content_type: &str,
) -> Result<()> {
    match std::fs::read(web_dir.join(name)) {
        Ok(body) => conn.respond_with(200, content_type, "Cache-Control: no-cache\r\n", &body),
        Err(_) => conn.respond(500, "text/plain", format!("web/{name} not found").as_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whitelist is the whole traversal guard on this route, so the two
    /// things it must do are: answer the names in the table, and answer nothing
    /// else. A `..` that resolved would reach the checkpoints directory.
    #[test]
    fn only_the_listed_names_resolve() {
        assert_eq!(lookup("/").unwrap().0, "index.html");
        assert_eq!(lookup("/js/app.js").unwrap().0, "js/app.js");
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
        let mut listed: Vec<String> = WEB_FILES.iter().map(|(n, _)| n.to_string()).collect();
        listed.sort();
        listed.dedup();
        assert_eq!(listed, found);
    }
}
