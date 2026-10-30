//! annex's core: instances in both modes, snapshots, windows, the desktop bridge, setup and
//! self-update. The `annex` CLI and the `annex-manager` window app are thin front ends over it.

pub mod adb;
pub mod bridge;
pub mod desktop;
pub mod docker;
pub mod doctor;
pub mod gpu;
pub mod instance;
pub mod launcher;
pub mod location;
pub mod paths;
pub mod process;
pub mod redroid;
pub mod scrcpy;
pub mod setup;
pub mod snapshot;
pub mod update;
pub mod waydroid;
pub mod window;
