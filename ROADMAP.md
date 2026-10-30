# Roadmap

## Where annex is going

A Linux developer runs `annex up`, gets an Android device in about 20 seconds, and uses it from Android Studio, Flutter and adb like any phone or emulator. It replaces the Android Studio emulator for day-to-day app work on Linux:

- Android runs in a container on the host kernel, not a virtual machine, so it boots, installs and launches several times faster and uses about a third of the memory.
- Instances freeze and thaw in a fifth of a second, and their data can be saved and restored under a label.
- Several instances, on different Android versions, run side by side.
- An instance is closed to other local users: only your adb key opens it.
- The instance feels like part of the desktop: shared clipboard, files and notifications, and app windows of its own.

Out of scope: Google Play and Play Integrity. Banking apps and some games need a real device or an emulator.

## Phase 0: prove the speed (done)

Benchmarked an Android Studio emulator against Redroid and Waydroid on the same machine. Gate: containers reach "ready" at least 3x faster than the emulator. Both passed: cold boot 3.9-6.1x, app launch about 15x, and the edit, install and launch loop about 9x. Details in [phase0/RESULTS.md](phase0/RESULTS.md).

## Phase 1: the CLI (done)

Goal: it boots, and Android Studio sees it.

- [x] `annex doctor`: kernel, binder (loaded now and at boot), Docker access, GPU, adb and its key, scrcpy, memory, default image
- [x] `annex up`, `down`, `ls`, `rm` on Redroid images for Android 13, 14 and 15, with adb on `127.0.0.1:5600` and up
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

## Next

- [ ] Android 16 (API 36+). Its system_server needs `/dev/ashmem`, which mainline kernels no longer have, so Redroid 16 does not boot. Needs a memfd patch to the image.
- [ ] Hardware rendering on NVIDIA GPUs (software rendering today)
- [ ] arm64 hosts: Redroid publishes arm64 images, but annex is only built and tested on x86_64
- [ ] Click a bridged notification to open its app. GNOME 50's notify-send no longer offers action buttons, so this needs the desktop notification API directly
- [ ] Snapshots for Waydroid, which needs a privileged helper because its data folder is owned by Android's uids
- [ ] Packages for Debian/Ubuntu, Fedora and Arch
