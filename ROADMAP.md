# Roadmap

## Where annex is going

A Linux developer runs `annex up`, gets an Android device in about 20 seconds, and uses it from Android Studio, Flutter and adb like any phone or emulator. It replaces the Android Studio emulator for day-to-day app work on Linux:

- Android runs in a container on the host kernel, not a virtual machine, so it boots, installs and launches several times faster and uses about a third of the memory.
- Instances freeze and thaw in a fifth of a second, and their data can be saved and restored under a label.
- Several instances, on different Android versions, run side by side.
- An instance is closed to other local users: only your adb key opens it.
- The instance feels like part of the desktop: shared clipboard, files and notifications, and app windows of its own.

Out of scope: Google Play itself and Play Integrity. A Google services stand-in covers maps, push messages and fused location (Phase 5); banking apps and some games still need a real device or an emulator.

## Phase 0: prove the speed (done)

Benchmarked an Android Studio emulator against Redroid and Waydroid on the same machine. Gate: containers reach "ready" at least 3x faster than the emulator. Both passed: cold boot 3.9-6.1x, app launch about 15x, and the edit, install and launch loop about 9x. Details in [phase0/RESULTS.md](phase0/RESULTS.md).

## Phase 1: the CLI (done)

Goal: it boots, and Android Studio sees it.

- [x] `annex doctor`: kernel, binder (loaded now and at boot), Docker access, GPU, adb and its key, scrcpy, memory, default image
- [x] `annex up`, `down`, `ls`, `rm` on Redroid images for Android 13, 14 and 15, with adb on `127.0.0.1:5600` and up (8.1 to 12 added in 0.8.0)
- [x] `annex freeze` / `thaw` (about 0.1 s each)
- [x] `annex snap save` / `snap ls` and `annex reset [--to <label>]`
- [x] `annex show`: a screen window through scrcpy
- [x] Security: adb accepts only the user's key, no extra listeners, private snapshots, older containers rebuilt in place
- [x] Configurable display size and density (`annex up --display 1080x2400/420`)
- [x] `annex install`, `push` and `pull`
- [x] Shell completions, CI, a release workflow and install instructions
- [x] `annex setup`: one command that checks the machine, shows a plan split by whether it needs sudo, asks once, and installs and configures everything with live progress
- [x] Optional instance names: `annex up` uses `default`; other commands use it or the only running instance

## Phase 2: desktop bridge (done)

A long-running process, `annex bridge`, run as a systemd user service:

- [x] Shared clipboard between the desktop and every running Redroid instance, both ways
- [x] Android notifications shown as desktop notifications
- [x] File transfer both ways (`annex push` / `pull`; one-shot commands, so they need no daemon)

The CLI still talks to Docker directly. A daemon in front of every command would add a failure point without making anything faster, so only the bridge runs in the background.

## Phase 3: native windows (done)

- [x] Window mode (`annex up <name> --mode window`), with each app in its own desktop window (`annex open <name> <package>`)
- [x] Per-app windows on Redroid too, through scrcpy virtual displays
- [x] The host's adb key written into Waydroid's trusted keys, so a new session needs no on-screen tap
- [x] Freeze and thaw for window mode without root, through its own system service

Window mode runs one instance at a time and only the Android version of its system image, so container mode stays the default.

## Phase 4: no terminal needed

Open, freeze and reset instances from the desktop. Each piece reuses the one before it.

- [x] App launcher entries: one per instance in the app grid and dock, with Freeze, Thaw, Stop and Reset to snapshot actions, reporting through notifications
- [x] Panel menu in the desktop bridge: every instance with its state, Show, Freeze or Thaw, Snapshot now, Reset to, Stop, and New instance
- [x] Annex Manager: a GTK 4 / libadwaita window app with instance cards, a detail page with snapshots and display settings, a New instance dialog, drag-and-drop APK install, and bridge settings. The core is a library target (`annex_core`) shared by the CLI and a second binary, `annex-manager`, built behind a `manager` feature so the CLI never needs GTK; `annex setup` installs it from the release

## Phase 5: CI and end-to-end tests

Run the same instances on CI runners, and give test suites what a real phone gives them.

- [x] A CI profile: `annex up --ephemeral --wait-boot --timezone --locale --animations off`, `annex serial`, `annex ls --json`, `annex doctor --json`, `annex setup --ci`, and container-aware `doctor`/`setup` for LXD runners
- [x] Network conditions: `annex net <name> online|offline|slow|lossy`
- [x] A virtual camera that apps see through Camera2: a back and a front camera filming a drawn picture, chosen per instance with `annex camera` (landscape, city, desk or test chart; Android 15, hardware rendering)
- [ ] Feed the camera from an image or a video of your choice, such as a QR code to scan
- [x] Evidence capture: `annex record` (screen video without a time limit) and `annex logs` (logcat to a file)
- [ ] A stable device id that survives `annex rm` and recreating an instance, for backends that bind a user to their first device
- [x] A Google services stand-in on new Android 15 instances: maps without an API key, placeholder push tokens with `annex fcm token|send`, and fused location
- [ ] The Google services stand-in on Android 13 and 14
- [x] Older Android: 8.1, 9, 10, 11 and 12
- [ ] `annex net offline` before Android 15, whose shells can't drive the firewall chain
- [ ] Simulated location before Android 12, through the older HIDL GNSS interface
- [ ] Prepare the Google services stand-in in `annex setup`, so the first `annex up` and fresh CI runners don't wait for it
- [ ] Choose Google services in Annex Manager's New instance dialog
- [ ] Real push messages through Google, as an opt-in per instance

## Phase 6: Windows and macOS

Instances share the host's Linux kernel, so on Windows and macOS they need one lightweight Linux VM underneath, shared by every instance. annex itself, adb and the screen window run natively and drive Docker inside that VM over its socket. Most instance code already only calls `docker` and connects adb to `127.0.0.1`; the parts that change are the host paths bind-mounted into each container, which must exist at the same path inside the VM, and the desktop pieces.

- [ ] Gate: on an Apple Silicon Mac, benchmark instances inside a Lima VM against the Android Studio emulator, which already runs on Apple's hypervisor there. Go ahead only if annex is still clearly faster at boot, resume and the install and launch loop, or clearly lighter on memory
- [ ] An engine layer: `docker.rs` targets the VM's Docker context, and `annex doctor` checks that every bind-mounted path exists inside the VM
- [ ] macOS: `annex setup` creates and starts a Lima VM (Apple Virtualization framework, Ubuntu guest, whose kernel has binder) with the home folder mounted at the same path and adb ports forwarded to the Mac. Apple Silicon runs arm64 images (see arm64 hosts below), so APKs built for phones run as they are
- [ ] Windows: the Linux `annex` binary inside WSL 2. Started: `annex doctor` and `annex setup` recognize WSL and give the steps only Windows can take (a WSL kernel with binder, systemd for the container engine). annex's own adb runs in WSL, so Windows needs neither Android Studio nor adb; with an adb on Windows, Windows tools such as Android Studio or flutter.exe reach instances too. Still to do: try it on a Windows machine
- [ ] Software rendering on both: a Mac's Linux VM gets no 3D acceleration, and WSL2 exposes `/dev/dxg` instead of `/dev/dri` render nodes. Features that need hardware rendering, such as the virtual camera, stay off there
- [ ] Machine checks and setup per OS: `setup/distro.rs`, `gpu.rs` and `doctor.rs` are Linux-only today
- [ ] Desktop bridge per OS: clipboard, notifications, panel menu, autostart (launchd, Windows Startup) and host location (CoreLocation, Windows geolocation) in place of X11, notify-send, the StatusNotifierItem menu, systemd and GeoClue
- [ ] App launcher entries per OS in place of `.desktop` files
- [ ] Release assets for macOS (arm64, x86_64) and Windows, CI runners for both, and `annex update` choosing the asset for its target
- [ ] Annex Manager on macOS and Windows. GTK 4 builds there, but libadwaita looks out of place, so the CLI ships first

Window mode stays Linux-only.

## Next

- [ ] Android 16 (API 36+). Its system_server needs `/dev/ashmem`, which mainline kernels no longer have, so Redroid 16 does not boot. Needs a memfd patch to the image.
- [ ] Hardware rendering on NVIDIA GPUs (software rendering today)
- [ ] arm64 hosts: Redroid publishes arm64 images, but annex is only built and tested on x86_64
- [ ] Click a bridged notification to open its app. GNOME 50's notify-send no longer offers action buttons, so this needs the desktop notification API directly
- [ ] Snapshots for Waydroid, which needs a privileged helper because its data folder is owned by Android's uids
- [ ] Packages for Debian/Ubuntu, Fedora and Arch
