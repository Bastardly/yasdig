#!/usr/bin/env bash
# Put a scan into a release's notes: a visible "VirusTotal" section, and the
# same facts as one JSON comment the site parses (site/index.html, `scanOf`).
# Everything between the two markers is replaced, so running it again on a
# release updates it rather than stacking a second copy; the notes above are
# left as written.
#
#   scan-notes.sh <tag> <scan.json>
#
# **The comment's shape is the site's input**: `<!-- yasdig-scan {…} -->` on
# one line, holding scan.json minified. Change both or neither.

set -euo pipefail
tag=$1
scan=$2
repo=${GITHUB_REPOSITORY:-Bastardly/yasdig}

BEGIN='<!-- yasdig-scan-begin -->'
END='<!-- yasdig-scan-end -->'

body=$(gh release view "$tag" --repo "$repo" --json body -q .body)
# The notes without any earlier scan section.
kept=$(awk -v b="$BEGIN" -v e="$END" '
  index($0, b) { skip = 1; next }
  index($0, e) { skip = 0; next }
  !skip' <<<"$body")

section=$(jq -r --arg repo "$repo" '
  def verdict: if .status != "completed" then "still being scanned"
    elif (.malicious + .suspicious) == 0 then "0 of \(.engines) engines flagged it"
    else "**\(.malicious + .suspicious) of \(.engines)** engines flagged it" end;
  "## VirusTotal",
  "",
  "Every release is scanned by [VirusTotal](https://www.virustotal.com) once it is published. Each report shows what every engine said, and stays live: engines update their verdicts over time.",
  "",
  "| File | Result | Report |",
  "|---|---|---|",
  (.files[] | "| `\(.name)`\(if .kind == "program" then " (inside the archive)" else "" end) | \(verdict) | [report](\(.report)) |"),
  "",
  "**SHA-256**",
  "",
  "```",
  (.files[] | select(.kind == "archive") | "\(.sha256)  \(.name)"),
  "```",
  "",
  "Check a download with `sha256sum -c SHA256SUMS` (Linux) or `certutil -hashfile <file> SHA256` (Windows), and where it was built with `gh attestation verify <file> --repo \($repo)`."
' "$scan")

comment="<!-- yasdig-scan $(jq -c . "$scan") -->"

{
  printf '%s\n\n' "$kept"
  printf '%s\n%s\n%s\n\n%s\n' "$BEGIN" "$comment" "$section" "$END"
} > notes.md

gh release edit "$tag" --repo "$repo" --notes-file notes.md
