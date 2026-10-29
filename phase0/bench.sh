#!/usr/bin/env bash
# Phase 0 benchmark: AVD vs Redroid vs Waydroid.
# Usage: bench.sh avd|redroid|waydroid [runs]   |   bench.sh summary
set -uo pipefail

DIR=$(cd "$(dirname "$0")" && pwd)
SDK=$HOME/Android/Sdk
ADB=$SDK/platform-tools/adb
EMU=$SDK/emulator/emulator
OUT=${OUT:-$DIR/results.jsonl}
APK=${APK:-$HOME/Projects/solutech-sat/build/app/outputs/flutter-apk/app-staging-debug.apk}
PKG=${PKG:-com.solutech.sat.v4.stg}
ACT=${ACT:-com.solutech.sat.v4.MainActivity}
AVD=annex_bench_api36
REDROID_IMAGE=${REDROID_IMAGE:-redroid/redroid:15.0.0-latest}
WAYDROID_SERIAL=192.168.240.112:5555

runs=${2:-3}
settle=${SETTLE:-60}
timeout=${TIMEOUT:-900}

now() { date +%s.%N; }
since() { awk -v a="$1" -v b="$(now)" 'BEGIN { printf "%.2f", b - a }'; }

die() {
  echo "[$1] $2, aborting" >&2
  exit 1
}

record() {
  printf '{"ts":"%s","runtime":"%s","metric":"%s","value":%s,"unit":"%s"}\n' \
    "$(date -Is)" "$1" "$2" "$3" "$4" | tee -a "$OUT"
}

wait_ready() {
  local serial=$1 start
  start=$(now)
  while :; do
    [[ $serial == *:* ]] && "$ADB" connect "$serial" >/dev/null 2>&1
    if [[ $("$ADB" -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r') == 1 ]] &&
      "$ADB" -s "$serial" shell pm path android >/dev/null 2>&1; then
      return 0
    fi
    (($(awk -v s="$(since "$start")" -v t="$timeout" 'BEGIN { print (s > t) }'))) && return 1
    sleep 0.25
  done
}

record_memory() {
  local runtime=$1 cg=$2
  sleep "$settle"
  local current swap anon
  current=$(cat "$cg/memory.current")
  swap=$(cat "$cg/memory.swap.current" 2>/dev/null || echo 0)
  anon=$(awk '$1 == "anon" { print $2 }' "$cg/memory.stat")
  record "$runtime" idle_memory_mb $((current / 1048576)) MB
  record "$runtime" idle_swap_mb $((swap / 1048576)) MB
  record "$runtime" idle_anon_mb $((anon / 1048576)) MB
}

deploy_loop() {
  local runtime=$1 serial=$2 start launch
  "$ADB" -s "$serial" uninstall "$PKG" >/dev/null 2>&1
  start=$(now)
  "$ADB" -s "$serial" install -g "$APK" >/dev/null || die "$runtime" "install failed"
  record "$runtime" install_first_s "$(since "$start")" s
  for i in $(seq "$runs"); do
    start=$(now)
    "$ADB" -s "$serial" install -r -g "$APK" >/dev/null || die "$runtime" "reinstall failed"
    record "$runtime" install_update_s "$(since "$start")" s
    "$ADB" -s "$serial" shell input keyevent KEYCODE_HOME
    "$ADB" -s "$serial" shell am start -W -S -n "$PKG/$ACT" | tr -d '\r' | tee -a "$DIR/launch-$runtime.log" >"$DIR/.launch"
    launch=$(awk -F': ' '/TotalTime/ { t = $2 } /WaitTime/ { w = $2 } END { print (t != "" ? t : w) }' "$DIR/.launch")
    record "$runtime" app_cold_launch_ms "${launch:-null}" ms
    record "$runtime" install_and_launch_s "$(since "$start")" s
    sleep 3
    "$ADB" -s "$serial" shell pidof "$PKG" >/dev/null || die "$runtime" "$PKG is not running after launch (crashed?)"
  done
  "$ADB" devices -l | grep -F "$serial" | sed "s/^/[$runtime] adb sees: /"
}

avd_launch() {
  setsid systemd-run --user --scope --collect --unit=annex-avd -q -- \
    "$EMU" -avd "$AVD" "$@" >>"$DIR/avd.log" 2>&1 &
  sleep 1
}

avd_stop() {
  "$ADB" -s emulator-5554 emu kill >/dev/null 2>&1
  while systemctl --user is-active -q annex-avd.scope 2>/dev/null; do sleep 1; done
}

avd_boot() {
  local metric=$1 start
  shift
  start=$(now)
  avd_launch "$@"
  wait_ready emulator-5554 || die avd "$metric: not ready after ${timeout}s"
  record avd "$metric" "$(since "$start")" s
}

bench_avd() {
  avd_stop
  avd_boot first_boot_s -wipe-data -no-snapshot-load
  avd_stop
  for _ in $(seq "$runs"); do
    avd_boot cold_boot_s -no-snapshot-load
    avd_stop
  done
  for _ in $(seq "$runs"); do
    avd_boot snapshot_resume_s
    avd_stop
  done
  bench_avd_tail
}

bench_avd_tail() {
  avd_boot snapshot_resume_s
  record_memory avd "/sys/fs/cgroup$(systemctl --user show -P ControlGroup annex-avd.scope)"
  deploy_loop avd emulator-5554
  avd_stop
}

redroid_cgroup() { echo "/sys/fs/cgroup/system.slice/docker-$(docker inspect -f '{{.Id}}' annex-redroid).scope"; }

redroid_ready() {
  local metric=$1 start=$2
  wait_ready localhost:5555 || die redroid "$metric: not ready after ${timeout}s"
  record redroid "$metric" "$(since "$start")" s
}

bench_redroid() {
  local start
  docker rm -f annex-redroid >/dev/null 2>&1
  start=$(now)
  docker run -d --privileged --name annex-redroid -p 127.0.0.1:5555:5555 "$REDROID_IMAGE" \
    androidboot.use_memfd=true androidboot.redroid_gpu_mode=host androidboot.redroid_width=1080 \
    androidboot.redroid_height=2400 androidboot.redroid_dpi=420 >/dev/null || die redroid "docker run failed"
  redroid_ready first_boot_s "$start"
  for _ in $(seq "$runs"); do
    docker stop annex-redroid >/dev/null
    "$ADB" disconnect localhost:5555 >/dev/null 2>&1
    start=$(now)
    docker start annex-redroid >/dev/null
    redroid_ready cold_boot_s "$start"
  done
  for _ in $(seq "$runs"); do
    docker pause annex-redroid >/dev/null
    sleep 2
    start=$(now)
    docker unpause annex-redroid >/dev/null
    redroid_ready freeze_resume_s "$start"
  done
  record_memory redroid "$(redroid_cgroup)"
  deploy_loop redroid localhost:5555
  docker stop annex-redroid >/dev/null
}

waydroid_cgroup() { find /sys/fs/cgroup -maxdepth 4 -type d -name 'lxc.payload.waydroid*' | head -1; }

waydroid_boot() {
  local metric=$1 start
  start=$(now)
  setsid waydroid session start >>"$DIR/waydroid.log" 2>&1 &
  wait_ready "$WAYDROID_SERIAL" || die waydroid "$metric: not ready after ${timeout}s"
  record waydroid "$metric" "$(since "$start")" s
}

waydroid_stop() {
  waydroid session stop >/dev/null 2>&1
  "$ADB" disconnect "$WAYDROID_SERIAL" >/dev/null 2>&1
  sleep 3
}

bench_waydroid() {
  waydroid_stop
  waydroid_boot first_boot_s
  waydroid_stop
  for _ in $(seq "$runs"); do
    waydroid_boot cold_boot_s
    waydroid_stop
  done
  waydroid_boot cold_boot_s
  record_memory waydroid "$(waydroid_cgroup)"
  deploy_loop waydroid "$WAYDROID_SERIAL"
  waydroid_stop
}

summary() {
  python3 -I - "$OUT" <<'PY'
import json, statistics, sys
from collections import defaultdict
rows = defaultdict(list)
for line in open(sys.argv[1]):
    r = json.loads(line)
    if r["value"] is not None:
        rows[(r["metric"], r["unit"])].append((r["runtime"], r["value"]))
runtimes = ["avd", "redroid", "waydroid"]
print(f"{'metric':24}" + "".join(f"{x:>12}" for x in runtimes))
for (metric, unit), values in sorted(rows.items()):
    cells = []
    for rt in runtimes:
        v = [x for r, x in values if r == rt]
        cells.append(f"{statistics.median(v):>10.1f}{unit[:2]:>2}" if v else f"{'-':>12}")
    print(f"{metric:24}" + "".join(cells))
PY
}

case ${1:-} in
  avd) bench_avd ;;
  avd-tail) bench_avd_tail ;;
  deploy-avd) avd_stop; avd_boot snapshot_resume_s; deploy_loop avd emulator-5554; avd_stop ;;
  deploy-redroid) docker start annex-redroid >/dev/null && redroid_ready cold_boot_s "$(now)" && deploy_loop redroid localhost:5555; docker stop annex-redroid >/dev/null ;;
  deploy-waydroid) waydroid_stop; waydroid_boot cold_boot_s; deploy_loop waydroid "$WAYDROID_SERIAL"; waydroid_stop ;;
  redroid) bench_redroid ;;
  waydroid) bench_waydroid ;;
  summary) summary ;;
  *) echo "usage: $0 avd|redroid|waydroid [runs] | summary" >&2; exit 2 ;;
esac
