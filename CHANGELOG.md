# Changelog

All notable changes to annex are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- `annex setup`: checks the machine, shows a plan split into steps that need sudo and steps that don't, asks once, then installs and configures everything with live progress (a byte progress bar with speed and ETA for the Android image). Safe to re-run. `--dry-run`, `--yes`, `--bridge`, `--window-mode` and `--repair`.
- Instance names are optional: `annex up` uses `default`, and other commands use `default` or the only running instance.
- App launcher entries for every instance, with Freeze, Thaw, Stop and Reset to snapshot actions, and an annex icon. `annex launcher install|remove`, and `annex open-window`, which starts an instance if needed and opens its screen.
- A panel menu in the desktop bridge listing every instance with its actions, plus New instance.
- `annex about` and `annex about --licenses`, and THIRD_PARTY_NOTICES.md.
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
- adb commands that can block (on a frozen or half-booted instance) now time out.
- `annex open` on Redroid switches the instance's adbd to root the first time, because Redroid's shell user can't create the trusted display scrcpy needs.

### Fixed

- `annex reset --to` failed with "device or resource busy" on instances created since adb keys were masked: the snapshot is now restored into the data volume before the container's masks are mounted.

## 0.1.0

First slice of the CLI: `doctor`, `up`, `down`, `ls`, `freeze`, `thaw`, `show`, `snap save`, `snap ls`, `reset` and `rm` for Redroid instances, with adb locked to the user's key, no extra listeners and private snapshots.
