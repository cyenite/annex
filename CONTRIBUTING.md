# Contributing to annex

Thanks for helping. annex is small on purpose: a thin, stateless layer over Docker, adb, scrcpy and Waydroid. The best contributions keep it that way.

## Ways to help

- **Report a bug.** Open an issue with the output of `annex doctor`, the command you ran and what happened. For bridge problems, include `annex bridge run --verbose` output; it logs sizes, never clipboard text.
- **Fix something.** Issues labelled `good first issue` are a good start. For anything larger than a bug fix, open an issue first, so we can agree on the approach before you spend time on it.
- **Measure.** Benchmarks on other hardware (AMD, NVIDIA, older laptops) make the numbers in the README honest. `phase0/bench.sh` is the harness.

Security problems go through [SECURITY.md](SECURITY.md), not public issues.

## Development setup

```sh
git clone https://github.com/cyenite/annex
cd annex
cargo build
cargo test
```

You need Rust 1.88 or newer. To build Annex Manager as well, install GTK 4 and libadwaita's development packages (`libgtk-4-dev libadwaita-1-dev` on Debian and Ubuntu, `gtk4-devel libadwaita-devel` on Fedora) and add `--features manager`:

```sh
cargo run --features manager --bin annex-manager
```

To click through the Manager without a desktop, for example from a remote session, run it on GTK's Broadway backend and open http://127.0.0.1:8085 in a browser:

```sh
gtk4-broadwayd :5 &
GDK_BACKEND=broadway BROADWAY_DISPLAY=:5 cargo run --features manager --bin annex-manager
``` To try your build against real instances you also need what `annex doctor` checks: binder, Docker, the Android SDK platform-tools, and scrcpy for windows and the bridge.

## Project layout

| Path | What it owns |
|---|---|
| `src/lib.rs` | The `annex_core` library: every module below, shared by both binaries |
| `src/main.rs` | The `annex` CLI: dispatches each command and prints its result |
| `src/cli.rs` | The command-line interface (clap) |
| `src/manager/` | Annex Manager (`--features manager`): window, instance cards, detail page, dialogs; work runs off the UI thread through `task::spawn` |
| `src/instance.rs` | The instance model: modes, states, Android versions, names, the default instance, dispatch to backends |
| `src/redroid.rs` | Redroid instances as Docker containers: create, rebuild, boot, display settings |
| `src/waydroid.rs` | Window mode, on Waydroid: start, freeze over D-Bus, state from its cgroup, app windows, the registered name |
| `src/snapshot.rs` | Snapshot save, list and reset (Redroid only) |
| `src/window.rs` | Screen and per-app windows for both backends |
| `src/launcher.rs`, `assets/annex.svg` | App launcher entries and the icon |
| `src/setup/` | `annex setup`: machine facts (shared with `doctor`), the plan, distro packages, image download, progress UI |
| `src/bridge/` | The desktop bridge: clipboard hub, X11 clipboard, scrcpy control protocol, notifications, panel menu, systemd unit |
| `src/adb.rs`, `src/docker.rs`, `src/scrcpy.rs` | Thin wrappers over the external tools |
| `src/process.rs` | Running commands with timeouts, detached processes, log tails |
| `src/doctor.rs`, `src/gpu.rs` | Machine checks |
| `src/update.rs` | `annex update`: release lookup, checksum, atomic binary swap, cargo fallback |
| `scripts/third-party-crates.sh` | Regenerates the crate table in `THIRD_PARTY_NOTICES.md` |
| `scripts/readme-images.py`, `docs/images/` | Renders the README's images from real captured output (usage in the script) |
| `phase0/` | The benchmark that motivated the design; not part of the build |

## Before you open a pull request

CI runs these, so run them first:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

If you touched `src/manager/` or anything it uses, also run `cargo clippy --all-targets --features manager -- -D warnings`.

Then try the change against a real instance. Unit tests cover parsing and protocol code; they can't cover Docker, adb or a desktop session. Describe what you ran in the pull request. For changes that touch these areas, check:

- **Lifecycle:** `annex up`, `freeze`, `thaw`, `down`, `rm` on a new instance.
- **Snapshots:** `snap save`, then `reset --to` brings an installed app back.
- **Bridge:** copy on the desktop and paste in an app, and the reverse; a notification posted with `adb shell cmd notification post` appears on the desktop.
- **Window mode:** `annex up desk --mode window` and `annex open desk com.android.settings`, if you have Waydroid.
- **Setup:** `annex setup --dry-run` on a machine missing something, and a real run in a fresh VM or container when you touch package installation.
- **Desktop:** the launcher entry opens and its actions work; the panel menu lists instances and its actions report by notification.

## Code style

The code is written to be read quickly. Match what's there:

- **Comments say why, never what.** A comment earns its place by explaining a workaround, an Android or Docker quirk, or a non-obvious ordering. No section banners, no restating the function name.
- **Name things for what they are.** Booleans read as predicates (`is_running`, `has_key`). Sibling fields get parallel names.
- **Group declarations by type, then order by name length**, shortest first, in struct fields and blocks of local variables.
- **Errors are actionable.** Every user-facing error says what to do next, like ``run `sudo modprobe binder_linux` ``. Use `anyhow` with context; never swallow an error silently.
- **Enums carry their own labels.** Order that matters (like states) lives in the enum's declaration order, not in a second list.
- **Never block forever on a device.** adb calls against a frozen instance hang, so anything that can run in the background uses a timeout (`process::output_within`).
- **No new state files.** Docker labels and the cgroup are the source of truth.
- **User-facing text speaks in annex's terms.** Help, errors, `doctor`, `setup`, `ls`, window titles, notifications and the panel menu say "container mode", "window mode" and "window helper", never Redroid, Waydroid or scrcpy. Code, comments, CONTRIBUTING and `--verbose` logs name them freely. Credit lives in `THIRD_PARTY_NOTICES.md`.
- **Plain punctuation in docs and messages:** hyphens, colons or two sentences, not em dashes.

## Commits and pull requests

- One logical change per pull request, with a description of the problem, the fix and how you tested it.
- Write commit messages in the imperative ("Add display option to up"), with a body explaining why when it isn't obvious.
- Add a line under `Unreleased` in [CHANGELOG.md](CHANGELOG.md) for anything a user would notice.
- New dependencies need a reason, and their licenses must be permissive. After changing dependencies, run `scripts/third-party-crates.sh` and update the table in `THIRD_PARTY_NOTICES.md`.

## Releases

1. In a pull request, set `version` in `Cargo.toml` (and run `cargo build` to update `Cargo.lock`), and turn the changelog's `Unreleased` section into `## X.Y.Z - YYYY-MM-DD` under a fresh, empty `Unreleased`.
2. After it merges, tag `vX.Y.Z` on `main` and push the tag.

The release workflow checks the tag matches `Cargo.toml`, builds the x86_64 binary, and attaches it to a GitHub release with the changelog section for that version. `annex update` installs it from there.

## Code of conduct

Be kind and assume good faith. Harassment or personal attacks of any kind are not tolerated; maintainers will remove comments, close threads or block accounts as needed.
