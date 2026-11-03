//! Network conditions for container-mode instances (`annex net`), for testing how apps behave
//! offline or on a poor connection.
//!
//! Offline uses Android's own per-app firewall (the OEM_DENY_3 chain) for every installed app, so
//! Android tells the apps they are blocked: `getActiveNetwork()` returns null and callbacks see
//! `onBlockedStatusChanged`, as on a phone with no signal. Dropping packets instead would leave
//! Android reporting a working network. Airplane mode doesn't help: it never turns Ethernet off.
//!
//! Slow and lossy shape the instance's interface with tc netem from inside its network namespace.
//! adb (TCP 5555) and ARP go through a band of their own that is never shaped, so annex, IDEs and
//! test runners keep their connection. Every condition lasts until the instance restarts.

use std::fs;
use std::path::Path;

use anyhow::{Result, bail};
use clap::ValueEnum;

use crate::docker;
use crate::instance::Instance;
use crate::redroid::container_name;

/// The traffic control pieces slow and lossy need. Most kernels load them on demand; a kernel
/// with module autoloading switched off needs them loaded first.
pub const MODULES: [&str; 6] = [
    "sch_prio",
    "sch_netem",
    "sch_ingress",
    "cls_u32",
    "act_gact",
    "act_police",
];
pub const MODULES_BOOT_FILE: &str = "/etc/modules-load.d/annex-net.conf";
const SLOW_NETEM: &str = "delay 300ms rate 1mbit";
const LOSSY_NETEM: &str = "delay 50ms loss 10%";
const SLOW_DOWNLOAD: &str = "rate 1mbit burst 64k";
/// Priorities of annex's filters on the ingress hook, far from any Android uses.
const ADB_INGRESS_PRIO: u32 = 49001;
const POLICE_INGRESS_PRIO: u32 = 49002;
const SHAPE_ERROR: &str = "RTNETLINK answers: No such file or directory";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Condition {
    /// The instance's normal connection
    Online,
    /// No network for any installed app; Android reports it as blocked
    Offline,
    /// About 1 Mbit/s each way with 300 ms of added latency, like weak 3G
    Slow,
    /// One packet in ten lost, with 50 ms of added latency
    Lossy,
}

impl Condition {
    pub fn label(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Slow => "slow",
            Self::Lossy => "lossy",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline (apps are told they have no network)",
            Self::Slow => "slow (about 1 Mbit/s each way, 300 ms added latency)",
            Self::Lossy => "lossy (10% of packets lost, 50 ms added latency)",
        }
    }
}

pub fn get(instance: &Instance) -> Result<Condition> {
    instance.container("network conditions")?;
    let container = container_name(&instance.name);
    let chain = docker::run(&[
        "exec",
        &container,
        "cmd",
        "connectivity",
        "get-chain3-enabled",
    ])?;
    let qdiscs = docker::run(&["exec", &container, "tc", "qdisc", "show", "dev", "eth0"])?;
    Ok(read_condition(&chain, &qdiscs))
}

pub fn set(instance: &Instance, condition: Condition) -> Result<()> {
    instance.container("network conditions")?;
    let container = container_name(&instance.name);
    if matches!(condition, Condition::Slow | Condition::Lossy) && !can_shape() {
        bail!(
            "this machine doesn't load the kernel's traffic shaping modules on demand: run `annex setup`, or `sudo modprobe -a {}`",
            MODULES.join(" ")
        );
    }
    let script = script(condition);
    match docker::run(&["exec", &container, "sh", "-c", &script]) {
        Err(error) if error.to_string().contains(SHAPE_ERROR) => bail!(
            "the kernel has no traffic shaping support for {}: run `sudo modprobe -a {}`",
            condition.label(),
            MODULES.join(" ")
        ),
        result => result.map(drop),
    }
}

/// Whether tc can create the qdiscs slow and lossy need: the modules are loaded, or the kernel
/// loads modules on demand (an empty `kernel.modprobe` switches that off).
pub fn can_shape() -> bool {
    let is_autoload_on =
        fs::read_to_string("/proc/sys/kernel/modprobe").is_ok_and(|path| !path.trim().is_empty());
    is_autoload_on
        || MODULES
            .iter()
            .all(|module| Path::new("/sys/module").join(module).exists())
}

/// One shell script that first clears any earlier condition, so switching between conditions
/// never stacks them. The clean-up commands may fail harmlessly; every later one must succeed.
fn script(condition: Condition) -> String {
    let cleanup = [
        "tc qdisc del dev eth0 root 2>/dev/null".to_string(),
        format!("tc filter del dev eth0 ingress prio {ADB_INGRESS_PRIO} 2>/dev/null"),
        format!("tc filter del dev eth0 ingress prio {POLICE_INGRESS_PRIO} 2>/dev/null"),
        "for p in $(pm list packages -3 | cut -d: -f2); do cmd connectivity set-package-networking-enabled true \"$p\" >/dev/null; done".to_string(),
        "cmd connectivity set-chain3-enabled false >/dev/null".to_string(),
    ];
    let mut steps = Vec::new();
    match condition {
        Condition::Online => {}
        Condition::Offline => steps.extend([
            "cmd connectivity set-chain3-enabled true >/dev/null".to_string(),
            "for p in $(pm list packages -3 | cut -d: -f2); do cmd connectivity set-package-networking-enabled false \"$p\" >/dev/null; done".to_string(),
        ]),
        // Android keeps a clsact qdisc of its own on eth0, which owns the ingress hook, so the
        // policing filters join it (creating it only where it's missing) and are removed by
        // priority; deleting the qdisc would take Android's BPF programs with it.
        Condition::Slow => {
            steps.extend(shaping(SLOW_NETEM));
            steps.extend([
                "(tc qdisc add dev eth0 clsact 2>/dev/null || true)".to_string(),
                format!("tc filter add dev eth0 ingress prio {ADB_INGRESS_PRIO} protocol ip u32 match ip protocol 6 0xff match ip dport 5555 0xffff flowid :1"),
                format!("tc filter add dev eth0 ingress prio {POLICE_INGRESS_PRIO} protocol ip u32 match u32 0 0 police {SLOW_DOWNLOAD} drop"),
            ]);
        }
        Condition::Lossy => steps.extend(shaping(LOSSY_NETEM)),
    }
    let mut script = cleanup.join("; ");
    for step in steps {
        script.push_str(" && ");
        script.push_str(&step);
    }
    script
}

/// Outgoing traffic goes to band 3, shaped by netem; adb replies and ARP go to band 1, which
/// isn't, so the instance stays reachable over adb.
fn shaping(netem: &str) -> [String; 4] {
    [
        "tc qdisc add dev eth0 root handle 1: prio bands 3 priomap 2 2 2 2 2 2 2 2 2 2 2 2 2 2 2 2".to_string(),
        format!("tc qdisc add dev eth0 parent 1:3 handle 30: netem {netem}"),
        "tc filter add dev eth0 parent 1: prio 1 protocol ip u32 match ip protocol 6 0xff match ip sport 5555 0xffff flowid 1:1".to_string(),
        "tc filter add dev eth0 parent 1: prio 2 protocol arp u32 match u32 0 0 flowid 1:1".to_string(),
    ]
}

fn read_condition(chain: &str, qdiscs: &str) -> Condition {
    if chain.trim() == "chain:enabled" {
        return Condition::Offline;
    }
    match qdiscs.lines().find(|line| line.contains("netem")) {
        Some(line) if line.contains("loss") => Condition::Lossy,
        Some(_) => Condition::Slow,
        None => Condition::Online,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_current_condition() {
        let plain = "qdisc noqueue 0: root refcnt 2";
        assert_eq!(read_condition("chain:disabled", plain), Condition::Online);
        assert_eq!(read_condition("chain:enabled\n", plain), Condition::Offline);
        let shaped = "qdisc prio 1: root refcnt 2 bands 3 priomap 2 2 2\nqdisc netem 30: parent 1:3 limit 1000 delay 300ms rate 1Mbit";
        assert_eq!(read_condition("chain:disabled", shaped), Condition::Slow);
        let lossy = "qdisc netem 30: parent 1:3 limit 1000 delay 50ms loss 10%";
        assert_eq!(read_condition("chain:disabled", lossy), Condition::Lossy);
    }

    #[test]
    fn every_condition_starts_by_clearing_the_last_one() {
        for condition in Condition::value_variants() {
            let script = script(*condition);
            assert!(script.starts_with("tc qdisc del dev eth0 root"), "{script}");
            assert!(script.contains("set-chain3-enabled false"), "{script}");
        }
        assert!(!script(Condition::Online).contains("&&"));
    }

    #[test]
    fn shaping_never_touches_adb() {
        for condition in [Condition::Slow, Condition::Lossy] {
            let script = script(condition);
            assert!(script.contains("match ip sport 5555 0xffff flowid 1:1"));
            assert!(script.contains("protocol arp u32 match u32 0 0 flowid 1:1"));
        }
        let slow = script(Condition::Slow);
        assert!(slow.contains("ingress prio 49001 protocol ip u32 match ip protocol 6 0xff match ip dport 5555 0xffff flowid :1"));
        assert!(!slow.contains("qdisc del dev eth0 ingress"));
    }

    #[test]
    fn offline_blocks_apps_instead_of_dropping_packets() {
        let script = script(Condition::Offline);
        assert!(script.contains("set-chain3-enabled true"));
        assert!(script.contains("set-package-networking-enabled false"));
        assert!(!script.contains("netem"));
    }
}
