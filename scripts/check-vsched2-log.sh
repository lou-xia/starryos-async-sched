#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
LOG_FILE=$(mktemp /tmp/starry-vsched2-log.XXXXXX)
trap 'rm -f "$LOG_FILE"' EXIT

VDSO_SO="$ROOT_DIR/vdso_vsched2_output/libvsched2.so"
if [[ ! -f "$VDSO_SO" ]]; then
    echo "missing vsched2 vDSO: $VDSO_SO"
    exit 1
fi

# Stage A only verifies the generic entries already provided by vsched2.  It
# does not install userspace VTABLEs or change the scheduler's runtime path.
DYNAMIC_SYMBOLS=$(readelf --dyn-syms --wide "$VDSO_SO")
for symbol in \
    raw_thread_entry raw_run_task raw_trap_entry \
    init_vtable_Task init_vtable_Stack init_vtable_Context \
    init_vtable_TrapInfo init_vtable_SMP init_vtable_VSpace init_vtable_UserData
do
    if ! rg -q "[[:space:]]${symbol}(@@vdso)?$" <<<"$DYNAMIC_SYMBOLS"; then
        echo "missing vsched2 vDSO symbol: $symbol"
        exit 1
    fi
done

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
require_log "[vsched2] kernel task accepted: name=dev-log-server"
require_log "[vsched2] kernel task accepted: name=alarm_task"
require_log "[vsched2] kernel task accepted: name=tty-reader"
require_log "sys_waitpid <="
require_log "[block_on] coroutine -> thread task="
require_log "trap handler pool grow: handler="
require_log "[block_on] thread -> coroutine task="
reject_log "panic in vDSO"
reject_log "memory allocation of"
reject_log "TrapHandler returned from a syscall before its block_on completed"
reject_log "block_on: invalid"
reject_log "block_on: resumed without"
reject_log "block_on: vsched2 is active before all block_on hooks are registered"
reject_log "block_on: vsched2 caller has no current scheduler task"
reject_log "wake_blocked_task: ready queue is full"
reject_log "trap handler already has an execution owner"
reject_log "trap_handler: trapped task state is"
reject_log "VSCHED2_TEST FAIL"
reject_log "VSCHED2_SHELL_WAIT4 FAIL"
reject_log "VSCHED2_INIT_TEST FAIL"

# `make verify-vsched2` can run on a rootfs that does not yet contain user
# tests.  When make test has copied vsched2_test into the image, require the
# complete user-space integration result as well.
if rg -a -F -q "VSCHED2_TEST START" "$LOG_FILE"; then
    require_log "VSCHED2_TEST user_vdso PASS"
    require_log "VSCHED2_TEST timer PASS"
    require_log "VSCHED2_TEST PASS"
    require_log "VSCHED2_SHELL_WAIT4 single PASS"
    require_log "VSCHED2_INIT_TEST PASS"
fi

echo "vsched2 log verification passed"
