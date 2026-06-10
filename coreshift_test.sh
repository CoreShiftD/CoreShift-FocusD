#!/system/bin/sh
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# coreshift_test.sh - Mimics the optimized foreground resolution logic

# 1. Get Cgroup v2 roots
V2_ROOTS=$(grep cgroup2 /proc/mounts | awk '{print $2}')
[ -z "$V2_ROOTS" ] && V2_ROOTS="/sys/fs/cgroup"

echo "Discovered Cgroup v2 roots: $V2_ROOTS"

# 2. Get top-app PIDs (sorted descending by PID for recency)
PIDS=$(cat /dev/cpuset/top-app/cgroup.procs | sort -rn)

echo "Candidates from top-app cpuset:"
for pid in $PIDS; do
    # 3. Filter by Cgroup v2 population
    V2_PATH=$(grep "0::" /proc/$pid/cgroup | cut -d: -f3)
    POPULATED=0
    for root in $V2_ROOTS; do
        if [ -f "$root$V2_PATH/cgroup.events" ]; then
            if grep -q "populated 1" "$root$V2_PATH/cgroup.events"; then
                POPULATED=1
                break
            fi
        fi
    done

    if [ $POPULATED -eq 1 ]; then
        UID=$(grep "^Uid:" /proc/$pid/status | awk '{print $2}')
        NAME=$(cat /proc/$pid/cmdline | tr '\0' ' ' | cut -d' ' -f1)
        OOM=$(cat /proc/$pid/oom_score_adj)
        echo "  PID: $pid | UID: $UID | OOM: $OOM | Name: $NAME"
    fi
done
