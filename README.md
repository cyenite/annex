# annex

Fast Android instances for Linux developers. An instance boots in about 20 seconds, freezes and thaws in a fifth of a second, and shows up in Android Studio, Flutter and adb as an ordinary device.

annex runs Android in a container on your own kernel instead of a virtual machine. On the Phase 0 test machine, the edit, install and launch loop was about 8x faster than an Android Studio emulator, and idle memory was about a third.

## Status

v0.1 in progress (Phase 1). Today annex manages Redroid-based instances from the command line. See [phase0/RESULTS.md](phase0/RESULTS.md) for the benchmark behind the design.

What it is not: there is no Google Play, so apps that need Play Integrity (banking, some games) will not run. Use a real device or an emulator for those.

## Requirements

- Linux with the `binder_linux` kernel module (`sudo modprobe binder_linux`)
- Docker, with your user in the `docker` group
- The Android SDK platform-tools (adb); scrcpy for `annex show`
- An Intel or AMD GPU for hardware rendering (NVIDIA falls back to software)

`annex doctor` checks all of this.

## Usage

```
annex doctor                     check this machine
annex up pixel                   create or start an instance (Android 15 by default)
annex up old --android 13        another instance on Android 13
annex ls                         list instances
annex show pixel                 open a window with the instance's screen
annex freeze pixel / thaw pixel  pause in memory and resume instantly
annex snap save pixel logged-in  save the instance's data under a label
annex reset pixel --to logged-in restore a snapshot (or `annex reset pixel` for first boot)
annex down pixel                 stop (data is kept)
annex rm pixel                   delete the instance and its data
```

Instances listen for adb on `127.0.0.1:5600` and up, outside the 5555-5585 range that adb scans for emulators.

## Security

Instances run as privileged containers (Redroid cannot boot otherwise), so they are root-equivalent on the host. annex keeps them closed to other local users:

- adbd only accepts your adb key (`~/.android/adbkey.pub`); any other key is `unauthorized`, including through the container's Docker bridge IP.
- Nothing else listens: the simulated Bluetooth service, which opens TCP ports, is disabled.
- Snapshots, which contain every app's private data, are readable only by you.

Anyone in the `docker` group can still open a shell in an instance, as with any container.

## Building

```
cargo build --release
```

## License

Apache-2.0
