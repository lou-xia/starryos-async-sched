#!/bin/sh

export HOME=/root

echo -e "Welcome to \e[96m\e[1mStarry OS\e[0m!"

SKIP_GROUPS="iperf netperf cyclictest ltp"

for script in /*_testcode.sh; do
    [ -f "$script" ] || continue
    group=$(basename "$script" _testcode.sh)
    skip=0
    for s in $SKIP_GROUPS; do
        [ "$s" = "$group" ] && skip=1 && break
    done
    echo "#### OS COMP TEST GROUP START $group ####"
    if [ $skip -eq 1 ]; then
        echo "SKIPPED: $group (not supported)"
    else
        sh "$script" || true
    fi
    echo "#### OS COMP TEST GROUP END $group ####"
done

echo "All tests completed."
exit 0
