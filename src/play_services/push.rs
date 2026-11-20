//! `annex fcm`: an app's offline push token, and test messages delivered the way Play services
//! delivers a real one, so the app's FirebaseMessagingService runs as it would for a server push.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

use super::PlayServices;
use crate::adb::{Adb, validate_package};
use crate::docker;
use crate::instance::{Backend, Instance};
use crate::redroid::container_name;

// Where AnnexPush records each app's token and sender id, one file per package.
const RECORDS: &str = "/data/data/com.google.android.gms/files/annex-push";
const SEND_TIMEOUT: Duration = Duration::from_secs(20);
// Keys FCM refuses in a message's data, because they mean something to the client library.
const RESERVED_KEYS: [&str; 4] = ["from", "notification", "message_type", "collapse_key"];
const RESERVED_PREFIXES: [&str; 2] = ["google", "gcm"];

/// What the stand-in recorded when it gave an app its token.
#[derive(Debug, PartialEq, Eq)]
pub struct Registration {
    pub token: String,
    pub sender: String,
}

/// A test message: data pairs, and an optional notification title and body.
#[derive(Default)]
pub struct Message {
    pub body: Option<String>,
    pub title: Option<String>,
    pub data: Vec<(String, String)>,
}

pub fn registration(instance: &Instance, package: &str) -> Result<Registration> {
    require_stand_in(instance)?;
    validate_package(package)?;
    let path = format!("{RECORDS}/{package}");
    let container = container_name(&instance.name);
    let missing = || {
        anyhow::anyhow!(
            "{package} has no push token on {} yet: it gets one when it calls FirebaseMessaging's getToken, which most apps do at startup",
            instance.name
        )
    };
    let record = docker::run(&["exec", &container, "cat", &path]).map_err(|_| missing())?;
    parse(&record).ok_or_else(missing)
}

pub fn send(adb: &Adb, instance: &Instance, package: &str, message: &Message) -> Result<()> {
    let registration = registration(instance, package)?;
    let sent = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string();
    let from = match registration.sender.as_str() {
        "" => "annex",
        sender => sender,
    };
    // Root holds every permission, including the one Firebase's receiver requires of the
    // sender (com.google.android.c2dm.permission.SEND), which only Play services has otherwise.
    let mut args: Vec<String> = [
        "su",
        "0",
        "am",
        "broadcast",
        "-a",
        "com.google.android.c2dm.intent.RECEIVE",
        "-p",
        package,
        "--es",
        "google.message_id",
        &format!("0:annex-{sent}"),
        "--el",
        "google.sent_time",
        &sent,
        "--es",
        "from",
        from,
    ]
    .map(String::from)
    .to_vec();
    if message.title.is_some() || message.body.is_some() {
        // Without this flag Firebase treats the message as data only and drops the title.
        args.extend(["--es", "gcm.n.e", "1"].map(String::from));
    }
    for (key, value) in [
        ("gcm.notification.title", &message.title),
        ("gcm.notification.body", &message.body),
    ] {
        if let Some(value) = value {
            args.extend(["--es".to_string(), key.to_string(), value.clone()]);
        }
    }
    for (key, value) in &message.data {
        args.extend(["--es".to_string(), key.clone(), value.clone()]);
    }
    let command: Vec<String> = args.iter().map(|arg| quote(arg)).collect();
    let output = adb.shell(&instance.serial, &command.join(" "), SEND_TIMEOUT)?;
    if !output.contains("result=-1") {
        bail!(
            "{package} didn't take the message ({}): it may not use Firebase Messaging, or it was force-stopped",
            output.lines().last().unwrap_or_default()
        );
    }
    Ok(())
}

/// Reads `--data KEY=VALUE` arguments, refusing the keys FCM itself refuses.
pub fn parse_data(pairs: &[String]) -> Result<Vec<(String, String)>> {
    pairs
        .iter()
        .map(|pair| {
            let Some((key, value)) = pair.split_once('=') else {
                bail!("{pair:?} is not KEY=VALUE");
            };
            let is_reserved = RESERVED_KEYS.contains(&key)
                || RESERVED_PREFIXES
                    .iter()
                    .any(|prefix| key.starts_with(prefix));
            if key.is_empty() || is_reserved {
                bail!("{key:?} can't be a data key: FCM reserves it");
            }
            Ok((key.to_string(), value.to_string()))
        })
        .collect()
}

fn require_stand_in(instance: &Instance) -> Result<()> {
    let has_stand_in = matches!(
        &instance.backend,
        Backend::Redroid(container) if container.play_services == PlayServices::StandIn
    );
    if !has_stand_in {
        bail!(
            "{0} has no Google services stand-in: `annex up {0} --play-services stand-in` adds it",
            instance.name
        );
    }
    Ok(())
}

fn parse(record: &str) -> Option<Registration> {
    let mut lines = record.lines();
    let token = lines.next().filter(|token| !token.is_empty())?;
    Some(Registration {
        token: token.to_string(),
        sender: lines.next().unwrap_or_default().to_string(),
    })
}

/// Quotes one argument for the device's shell.
fn quote(argument: &str) -> String {
    format!("'{}'", argument.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_token_record() {
        let record = parse("fid:annex_offline_ab\n1234567890\n").unwrap();
        assert_eq!(record.token, "fid:annex_offline_ab");
        assert_eq!(record.sender, "1234567890");
        assert_eq!(parse("token\n").unwrap().sender, "");
        assert!(parse("").is_none());
    }

    #[test]
    fn reads_data_pairs_and_refuses_reserved_keys() {
        let pairs = ["orderId=42".to_string(), "note=a=b".to_string()];
        assert_eq!(
            parse_data(&pairs).unwrap(),
            [
                ("orderId".to_string(), "42".to_string()),
                ("note".to_string(), "a=b".to_string())
            ]
        );
        for bad in ["from=x", "google.c.a=1", "gcm.n.e=1", "=x", "novalue"] {
            assert!(parse_data(&[bad.to_string()]).is_err(), "{bad}");
        }
    }

    #[test]
    fn quotes_for_the_device_shell() {
        assert_eq!(quote("Order ready"), "'Order ready'");
        assert_eq!(quote("it's $HOME"), r"'it'\''s $HOME'");
    }
}
