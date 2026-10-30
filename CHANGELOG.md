# Changelog

All notable changes to annex are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## Unreleased

## 0.3.0 - 2026-10-30

### Added

- Annex Manager (`annex-manager`), a GTK 4 / libadwaita window app: instance cards with live actions, a page per instance with its details, screen size and snapshots (save, restore, delete), a New instance dialog, drag-and-drop APK install, and settings for the bridge and autostart. Open it from the "Annex" launcher entry, the panel menu or `annex manager`.
- `annex setup` installs Annex Manager on a desktop from the release matching its version and adds its launcher entry; `annex update` updates it with annex. Releases publish it as a second archive.
- `annex setup --no-bridge` and `--no-manager`.

### Changed

- `annex setup` sets up the desktop bridge by default in a desktop session; `--bridge` is no longer needed.
- Instances appear in IDEs as "Annex <Name> <version>" instead of the image's "redroid15 x86 64"; existing instances pick it up on their next `annex up` (data kept).
- The release archive includes THIRD_PARTY_NOTICES.md.

## 0.2.0 - 2026-10-30

### Added

- `annex setup`: checks the machine, shows a plan split into steps that need sudo and steps that don't, asks once, then installs and configures everything with live progress (a byte progress bar with speed and ETA for the Android image). Safe to re-run. `--dry-run`, `--yes`, `--bridge`, `--window-mode` and `--repair`.
- Instance names are optional: `annex up` uses `default`, and other commands use `default` or the only running instance.
- App launcher entries for every instance, with Freeze, Thaw, Stop and Reset to snapshot actions, and an annex icon. `annex launcher install|remove`, and `annex open-window`, which starts an instance if needed and opens its screen.
- A panel menu in the desktop bridge listing every instance with its actions, plus New instance.
- `annex about` and `annex about --licenses`, and THIRD_PARTY_NOTICES.md.
- `annex update` installs the latest release after verifying its SHA-256 (or rebuilds the latest source while none is published), then restarts the bridge; `--check` only looks.
- `annex doctor` checks for a panel that can show the bridge's menu.
- `annex up --display WIDTHxHEIGHT/DPI` sets an instance's screen; changing it on an existing instance rebuilds the container and keeps its data.
- `annex install`, `annex push` and `annex pull` for APKs and files.
- `annex open <instance> <package>` opens one app in a desktop window of its own: a native window on Waydroid, a scrcpy virtual display on Redroid.
- Window mode (`annex up <name> --mode window`): one instance whose apps open in native desktop windows, with start, stop, freeze and thaw without root, and your adb key authorized automatically.
- The desktop bridge (`annex bridge run`, `install`, `uninstall`, `status`): one clipboard across the desktop and every running Redroid instance, and Android notifications as desktop notifications. It runs as a systemd user service.
- `annex completions <shell>` prints shell completions.
- `annex doctor` checks scrcpy's version and server, Waydroid, the desktop clipboard and notify-send.
- CI (format, clippy, tests) and a release workflow that publishes a Linux binary.

### Changed

- `annex ls` shows each instance's mode (container or window).
- Messages, help and `doctor` describe modes and capabilities instead of naming the projects behind them.
- `annex doctor` colours its status labels on a terminal (plain when piped or with NO_COLOR).
- `annex up` opens the instance's window once it's ready, unless one is already open, `--no-window` is given, or there is no desktop session.
- `annex open-window` (launchers) no longer stacks a second window on an instance that has one.
- adb commands that can block (on a frozen or half-booted instance) now time out.
- `annex open` on Redroid switches the instance's adbd to root the first time, because Redroid's shell user can't create the trusted display scrcpy needs.

### Fixed

- Each running instance added about 36 drives to the dock and file manager: Android mounts its system modules through the host's loop devices. `annex setup` now installs a udev rule that hides them, and `annex doctor` reports when it's missing.
- `annex reset --to` failed with "device or resource busy" on instances created since adb keys were masked: the snapshot is now restored into the data volume before the container's masks are mounted.

## 0.1.0

First slice of the CLI: `doctor`, `up`, `down`, `ls`, `freeze`, `thaw`, `show`, `snap save`, `snap ls`, `reset` and `rm` for Redroid instances, with adb locked to the user's key, no extra listeners and private snapshots.
