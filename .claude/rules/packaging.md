---
paths:
  - "build.rs"
  - "src/bundle.rs"
  - "src/home.rs"
  - "src/startup.rs"
  - ".github/**/*"
  - "packaging/**/*"
  - "patches/**/*"
  - "site/**/*"
  - "llm/CMakeLists.txt"
  - "CHANGELOG.md"
  - "Cargo.toml"
  - "THIRD_PARTY_LICENSES.md"
---

# Packaging, releasing, the website

## Packaging

A release is **one executable** per OS with sd-cli and the page inside it
(`.github/workflows/release.yml` on a `v*` tag), shipped as an **archive holding a
`yasdig/` folder** (the program plus `packaging/README.txt`): a `.zip` on Windows
(Edge's SmartScreen warns far less than for a bare unsigned `.exe`), a `.tar.gz` on
Linux (keeps the executable bit). Double-clicked, it creates its folders beside
itself, starts, opens the browser. Not Tauri (WebKitGTK would be a dependency).
**`cargo build --release` is the whole recipe.** sd.cpp is a gitignored clone at
`./stable-diffusion.cpp/`; `build.rs` builds `sd-cli` into `build-yasdig/` there and a
*release* build embeds it (`build.rs` → `src/bundle.rs`, unpacked to `bin/sd-cli` on
start). Debug builds never embed (link time); a failed sd.cpp build warns in debug,
errors in release. `YASDIG_SD_CLI=<path>` overrides. The repo's `models.toml` names
`build-yasdig/bin/sd-cli`.

**sd.cpp is patched**: `build.rs` applies `patches/stable-diffusion.cpp/*.patch`
(`apply_patches`: applied, or skipped when its reverse applies, else a hard error —
a bump of `SD_CPP_REF` must rebase them). Today one: LTXAV takes `--control-video`
with `--hires` as the first pass (`latents.from_control_video`). Regenerate a patch
with `git -C stable-diffusion.cpp diff -- <files>`.

`build.rs` traps: it **reruns on `.git/logs/HEAD`, not the source tree** (hand edit to
sd.cpp needs `touch build.rs`); **configure is stamped on success**
(`.yasdig-configured`), not judged by `CMakeCache.txt`.

Five things break silently:

- **Home folder is `home::config_path`**: an argument, else `models.toml` in the CWD,
  else this checkout when the exe is inside its `target/`, else the exe's folder —
  never the CWD alone. `home::ensure` creates only what is missing. **An exe folder
  inside `temp_dir()` is refused** (`in_temp`): double-clicking it inside the zip runs
  it from a folder Windows deletes, models and all.
- **`[sd].binary` wins over the bundle**; `Config::load` unpacks only when it's unset.
- **Unpack is keyed on the stamp `bin/sd-cli.version`** (build.rs's hash), not on
  hashing 93 MB. Binary goes through `.part` + rename, stamp written last.
- **The page is served from `web/` when present, else `WEB_FILES`** (`web::serve`).
  `doctor` checks files on disk only when it reads them.
- **The listener binds before the GPU worker starts** (`Engine::new` sweeps `work/`).
  Port is `startup.toml`'s (`src/startup.rs`). A taken port is asked `GET /instance`;
  `startup::bind`: same folder *and* build → hand over (open browser, exit); any other
  yasdig → `POST /shutdown {"clear":false}` and take the port; non-yasdig → never
  touched, try the next of 20. `"app":"rustimage"` is accepted (transitional).
  **`build_id` is read once, first thing in `run`** (re-read, a rebuilt binary hands
  over to a stale server). **Takeover must send `clear:false`**.

**A master push builds Windows alone** (same `release.yml`, the Linux job skipped,
`publish`/`scan` tag-only): a 14-day workflow artifact to try on a Windows PC, and
`cargo test` on Windows. Never a rolling pre-release — the site's buttons read
`/releases` with pre-releases included.

**Releasing** (`v*` tag → `release.yml`, ~9 min; pushing `master` also deploys the
site when `site/` changed):

1. Commit the work. In `CHANGELOG.md` turn `## Unreleased` into `## X.Y.Z — date`;
   bump `version` in `Cargo.toml` and `yasdig`'s entry in `Cargo.lock`; `cargo build`;
   commit "Release vX.Y.Z"; `git tag -a vX.Y.Z -m "yasdig vX.Y.Z"`.
2. `git push origin master vX.Y.Z`.
3. **Write the release page's notes** — never leave only GitHub's generated
   "Full Changelog" link. The version's CHANGELOG section, plus a link to the full
   changelog, into a file, then **right after the push**
   `gh release create vX.Y.Z --verify-tag --title vX.Y.Z --notes-file <file>`: the
   workflow's publish job finds the release and only uploads the archives to it,
   keeping title and notes. If it already ran, `gh release edit vX.Y.Z --notes-file`.
4. Watch it (`gh run watch`); check both archives are on the release, and — about
   20 minutes later — the *VirusTotal* section (Linux only) in its notes.

**Trust without a certificate**: `publish` also writes `SHA256SUMS` and a build
provenance attestation (`actions/attest-build-provenance`); then the `scan` job
(`.github/workflows/scan.yml`, reusable, also runnable by hand with a tag) uploads
the **Linux** archive and the program inside it to VirusTotal — **Windows isn't
scanned** (dropped 2026-11-01: an unsigned exe drew a 1/69 guess-flag every release
and no certificate is in reach; old releases' notes keep theirs)
(`.github/scripts/virustotal.sh`: bash, curl, jq; secret **`VT_API_KEY`**, Public API
at 4 calls/min) and appends the result to the release notes
(`scan-notes.sh`, between `<!-- yasdig-scan-begin/end -->`, replaced on a re-run, the
notes above kept), plus `scan.json`. It runs after the release is public and never
blocks one.

- **The site parses that notes section**: one `<!-- yasdig-scan {scan.json} -->`
  comment (`scanOf` in `site/index.html`). Change both or neither.
- **The hash shown is GitHub's own `digest` of the asset**, there from upload,
  not the scan's.
- **The README's VirusTotal badge reads `https://yasdig.com/badge.json`** (shields.io
  endpoint format). **shields.io refuses github.com URLs**, so `pages.yml` copies the
  newest release's `badge.json` into the site on every deploy, and `scan.yml` starts
  a deploy when it is done (`actions: write`).

`build.rs` is a table of two native builds (`BUILDS`): sd.cpp's `sd-cli`, and the
agent's `yasdig-llm` from our `llm/` against a gitignored `./llama.cpp/` clone (into
`llama.cpp/build-yasdig/`, `cfg(bundled_llm)`, `YASDIG_LLM`). A missing llama.cpp clone
is a warning even in release — the build then has no agent. `src/bundle.rs` is the same
table (`bundle::SD`, `bundle::LLM`). CI pins `LLAMA_CPP_REF` beside `SD_CPP_REF`.

sd.cpp flags in `build.rs` (`GGML_NATIVE=OFF`, `GGML_OPENMP=OFF`, `/MT`) make it
portable; the workflow fails if sd-cli links anything but the Vulkan loader and libc.
Static libstdc++ is a workflow `LDFLAGS` (Fedora lacks `libstdc++.a`; CI's Ubuntu 22.04
is what travels). Model-folder names shown to the page go through `config::slash_name`
so they read `sdxl/x` on Windows too.

## Website, licences

- **Video left nightly**: `nightly_video` is gone from `Settings` (an old file
  with it still loads — no `deny_unknown_fields` — and the key drops on the next save).
  A future unfinished feature can bring the section back.
- **Website**: `site/` (yasdig.com), deployed by `.github/workflows/pages.yml`, kept
  out of `docs/`. Short and download-first; tokens hand-copied from `web/style.css`.
  Buttons repoint via `api.github.com/…/releases` (not `releases/latest`, which skips
  pre-releases); failure leaves the Releases link. **Both buttons open a dialog
  first** (`#win-dialog`, `#linux-dialog`, `askFirst`): how to start it, then a
  *Checked* block (`checked()`): version, VirusTotal's verdicts (Linux; the Windows
  dialog has no line when the scan lists no Windows file), the SHA-256 with a
  copy button, and — when `api.github.com/…/attestations/<digest>` has one — the
  build attestation. Asset names
  (`yasdig-windows-x86_64.zip`, `yasdig-linux-x86_64.tar.gz`; the regexes still match
  the bare files older releases carry) must match `release.yml`. The Windows dialog
  also says what SmartScreen will show and why; a dialog opens only once the script
  found the file, and its own link downloads.
- **Licences**: Apache-2.0. `THIRD_PARTY_LICENSES.md` (sd.cpp, ggml and their bundled
  libs verbatim, plus the crate table from `cargo tree -e normal` × `cargo metadata`)
  is compiled in and served at `GET /licences`. **Regenerate on an sd.cpp or llama.cpp bump or a new
  crate.**
