#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
LOG_FILE=$(mktemp /tmp/starry-vsched2-log.XXXXXX)
trap 'rm -f "$LOG_FILE"' EXIT

set +e
timeout "${VSCHED2_TEST_TIMEOUT:-15s}" make -C "$ROOT_DIR" justrun >"$LOG_FILE" 2>&1
STATUS=$?
set -e

if [[ $STATUS -ne 0 && $STATUS -ne 124 ]]; then
    tail -n 100 "$LOG_FILE"
    exit $STATUS
fi

require_log() {
    if ! rg -a -F -q "$1" "$LOG_FILE"; then
        echo "missing vsched2 log: $1"
        tail -n 100 "$LOG_FILE"
        exit 1
    fi
}

reject_log() {
    if rg -a -F -q "$1" "$LOG_FILE"; then
        echo "unexpected vsched2 log: $1"
        tail -n 100 "$LOG_FILE"
        exit 1
    fi
}

require_log "Welcome to"
require_log "[wait4] PENDING task="
require_log "[wait4] WAKE task="
require_log "path=./hello_world"
require_log "Hello, World!"
reject_log "panic in vDSO"
reject_log "memory allocation of"

echo "vsched2 log verification passed"
