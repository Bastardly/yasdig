#!/usr/bin/env sh
# yasdig.com on this computer: http://127.0.0.1:8090/ (or the port given).
# Pages rebuild on reload; ?mock=flagged etc. shows the download dialog states.
# Ctrl+C stops it. Same as `node site/serve.mjs [port]`.
cd "$(dirname "$0")" && exec node site/serve.mjs "$@"
