#!/usr/bin/env bash
# Scan release files with VirusTotal and print what it found, as JSON, for
# `scan.yml` to put in the release (the site reads it from there — see
# `scan-notes.sh`). Bash, curl and jq: no Python anywhere in the build.
#
#   VT_API_KEY=… virustotal.sh <kind>:<os>:<file> …
#
# kind is "archive" (what is downloaded) or "program" (what runs, taken out of
# the archive); os is "windows" or "linux". One JSON object per file on stdout:
#   {name, kind, os, sha256, size, status, malicious, suspicious, engines,
#    scanned, report}
#
# The free Public API allows 4 requests a minute, so every call waits its turn
# (`vt`), and files go one at a time. A file VirusTotal already knows is not
# uploaded again: its last analysis is used. Our files are 34–90 MB, past the
# 32 MB direct upload, so each goes through an upload URL.

set -euo pipefail

API=https://www.virustotal.com/api/v3
: "${VT_API_KEY:?set VT_API_KEY (a repository secret)}"
# How long to wait for one file's analysis before reporting it as pending.
WAIT_SECONDS=${VT_WAIT_SECONDS:-1800}

last_call=0
# One API call, no sooner than 16 s after the last: 4 a minute at most.
vt() {
  local now; now=$(date +%s)
  local wait=$(( last_call + 16 - now ))
  (( wait > 0 )) && sleep "$wait"
  last_call=$(date +%s)
  curl -sS --retry 3 --retry-delay 20 -H "x-apikey: $VT_API_KEY" "$@"
}

# The file's report, or nothing when VirusTotal hasn't seen it.
report() {
  local out; out=$(vt -w '\n%{http_code}' "$API/files/$1")
  local code=${out##*$'\n'}
  if [[ $code == 200 ]]; then printf '%s' "${out%$'\n'*}"; fi
}

for spec in "$@"; do
  IFS=: read -r kind os file <<<"$spec"
  name=$(basename "$file")
  sha=$(sha256sum "$file" | cut -d' ' -f1)
  size=$(stat -c %s "$file")
  echo "::group::$name ($sha)" >&2

  rep=$(report "$sha")
  if [[ -z $rep || $(jq '.data.attributes.last_analysis_date // 0' <<<"$rep") == 0 ]]; then
    echo "uploading $name ($size bytes)" >&2
    url=$(vt "$API/files/upload_url" | jq -r .data)
    analysis=$(vt -F "file=@$file" "$url" | jq -r .data.id)
    deadline=$(( $(date +%s) + WAIT_SECONDS ))
    status=queued
    while (( $(date +%s) < deadline )); do
      sleep 30
      status=$(vt "$API/analyses/$analysis" | jq -r .data.attributes.status)
      echo "  $status" >&2
      [[ $status == completed ]] && break
    done
    rep=$(report "$sha")
  else
    echo "already known to VirusTotal: using its last analysis" >&2
    status=completed
  fi
  echo "::endgroup::" >&2

  # `engines` counts the ones that gave a verdict: not "type unsupported",
  # timeouts or failures, which say nothing about the file.
  jq -n --arg name "$name" --arg kind "$kind" --arg os "$os" --arg sha "$sha" \
     --argjson size "$size" --arg status "$status" --argjson rep "${rep:-null}" '
    ($rep.data.attributes // {}) as $a
    | ($a.last_analysis_stats // {}) as $s
    | {
        name: $name, kind: $kind, os: $os, sha256: $sha, size: $size,
        status: (if $a.last_analysis_date then $status else "pending" end),
        malicious: ($s.malicious // 0), suspicious: ($s.suspicious // 0),
        engines: (($s.malicious // 0) + ($s.suspicious // 0) + ($s.undetected // 0) + ($s.harmless // 0)),
        scanned: (if $a.last_analysis_date then ($a.last_analysis_date | todate) else null end),
        report: ("https://www.virustotal.com/gui/file/" + $sha)
      }'
done
