# Phase 0 results: AVD vs Redroid vs Waydroid

Machine: i7-1365U (12 threads), 14 GB RAM, Intel UHD (Raptor Lake-P, Mesa), Ubuntu 26.04, kernel 7.0.0-38.
Test app: `solutech-sat` development debug APK (137 MB, Flutter, x86_64 libs), frozen as `test.apk`.
Run: 2026-10-29 11:30-12:20, all three back to back, 5-6.6 GB RAM available, medians of 3 runs (`results.jsonl`).

| Metric | AVD (API 36.1, Play, 2 GB) | Redroid 15 | Waydroid (LineageOS 20, Android 13) |
|---|---|---|---|
| First boot | 180 s | 19.1 s (9.4x) | 11.0 s (16x) |
| Cold boot | 70.7 s | 17.9 s (3.9x) | 11.5 s (6.1x) |
| Resume | 6.6 s (snapshot) | 0.16 s (unfreeze, 41x) | not measured (freeze needs root) |
| Idle memory, anonymous | 1.39 GB | 0.53 GB | 0.50 GB |
| Idle memory, cgroup total | 2.0 GB | 1.1 GB | 2.2 GB (mostly reclaimable image page cache) |
| Reinstall APK (`install -r -g`) | 28.5 s | 1.0 s (28x) | 1.3 s (22x) |

The launch rows of this run are left out: that APK has no `lib/x86_64/libflutter.so` (it was built for an arm64 phone), so the app crashed during startup on all three and the timings measured a crash. Launch was re-measured with an APK that runs:

### Launch, re-measured (2026-10-29 14:00)

App: `shorecat` code-push example, Flutter release APK, 46 MB, with x86_64 engine. Medians of 3; the harness now checks the app's process is alive 3 s after each launch (`results-launch.jsonl`).

| Metric | AVD | Redroid 15 | Waydroid |
|---|---|---|---|
| App cold launch (`am start -W -S`) | 8.8 s | 0.60 s (15x) | 0.54 s (16x) |
| Reinstall APK | 6.4 s | 0.78 s (8x) | 0.90 s (7x) |
| Reinstall + launch loop | 17.3 s | 2.0 s (8.7x) | 1.9 s (9x) |

**Gate (3x faster to "ready" than an AVD): passed by both containers.** Cold boot is 3.9x (Redroid) and 6.1x (Waydroid) faster; app launch is about 15x faster and the edit, install and launch loop about 9x.

All three show up in `adb devices` as normal devices, so Android Studio and Flutter list them. Redroid renders on the host Intel GPU (`Mesa Intel(R) Graphics (RPL-U), OpenGL ES 3.2`, screenshot `redroid-app.png`).

## Caveats

- Android versions differ (AVD 36.1, Redroid 15, Waydroid 13). No API 35 AVD image is installed.
- The AVD has a fixed 2 GB guest and a window; the containers have no memory limit, and only Waydroid's window was opened (for adb approval).
- The re-measured launch used a smaller app than `solutech-sat`; a `solutech-sat` debug build with an x86_64 engine would give a like-for-like number.
- Waydroid's first-ever install after `waydroid init` took 276 s. A retest with two fresh installs took 1.04 s and 0.88 s, so it is excluded from the conclusions.
- An earlier AVD run under heavy swap (2.5 GB available, 29 GB swap) was slower still: cold boot 124 s, reinstall 41 s (`run1/`).

## Findings that matter for the product

1. **Redroid 16 does not boot on a mainline kernel.** Android 16's `ApplicationSharedMemory` in system_server opens `/dev/ashmem` directly and ignores `use_memfd`; mainline has no ashmem (`run1/results-invalid-redroid16.jsonl`). Images for API 36+ need a memfd patch.
2. **Redroid 15 needs `androidboot.use_memfd=true`** on this kernel.
3. **adb port collision:** Redroid on host port 5555 appears twice (`emulator-5554` and `localhost:5555`), because adb treats ports 5555-5585 as emulators. Use other ports or a Unix socket.
4. **Waydroid asks for adb key approval on screen** on first connect. Images must ship with the host key pre-authorized, or every new instance blocks on a tap and headless/CI use is impossible.
5. **Waydroid's image download relies on SourceForge**, which stalled at 0 kB/s from this network (the auto-picked mirror). `fetch-waydroid-images.sh` adds resume, mirror rotation and checksum checks; images go in `/etc/waydroid-extra/images`, then `waydroid init -f`.
6. **Binder is a module on Ubuntu** (`binder_linux`) and is not loaded after a reboot. Setup (`annex doctor`) must load it and make it persistent.
7. Waydroid's container freeze needs root, so instant resume needs a privileged helper; Redroid gets it free via `docker pause`.

## Files

- `bench.sh`: harness (`./bench.sh avd|redroid|waydroid [runs]`, `./bench.sh deploy-avd|deploy-redroid|deploy-waydroid`, `./bench.sh summary`; `APK`, `PKG`, `ACT`, `TIMEOUT`, `OUT` env overrides)
- `fetch-waydroid-images.sh`: resumable, checksum-verified Waydroid image download
- `results.jsonl`: the comparable three-way run (launch rows invalid, see above)
- `results-launch.jsonl`: the launch re-measurement
- `launch-*.log`: raw `am start -W` output; the first three entries per file are the three-way run (crashing APK), the last three the re-measurement
- `run1/`, `run2/`: earlier runs (heavy-swap AVD run, invalid Redroid 16 run, AVD vs Redroid with the staging APK)
