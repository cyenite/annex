use std::collections::HashMap;
use std::io::Write;
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use anyhow::{Result, bail};

use super::host::HostClipboard;
use super::protocol::set_clipboard;
use super::trace;

pub enum Event {
    Host(String),
    HostFailed(String),
    Device { name: String, text: String },
    Attach { name: String, control: TcpStream },
    Detach { name: String },
}

/// Starts the thread that keeps one clipboard across the desktop and every attached instance.
/// It returns an error, ending the bridge, only if the desktop clipboard connection breaks.
pub fn spawn() -> Result<(Sender<Event>, JoinHandle<Result<()>>)> {
    let (events, inbox) = mpsc::channel();
    let host = match HostClipboard::start(events.clone()) {
        Ok(host) => Some(host),
        Err(error) => {
            eprintln!("desktop clipboard unavailable, syncing between instances only: {error:#}");
            None
        }
    };
    let hub = thread::Builder::new()
        .name("clipboard-hub".to_string())
        .spawn(move || Hub::new(host).run(inbox))?;
    Ok((events, hub))
}

struct Hub {
    /// The text every side was last given, so echoes of our own writes stop here.
    last: Option<String>,
    host: Option<HostClipboard>,
    devices: HashMap<String, TcpStream>,
}

impl Hub {
    fn new(host: Option<HostClipboard>) -> Self {
        Self {
            last: None,
            host,
            devices: HashMap::new(),
        }
    }

    fn run(mut self, inbox: Receiver<Event>) -> Result<()> {
        for event in inbox {
            match event {
                Event::Host(text) => self.share(None, text),
                Event::Device { name, text } => self.share(Some(&name), text),
                Event::HostFailed(error) => bail!("desktop clipboard connection lost: {error}"),
                Event::Attach { name, mut control } => {
                    if let Some(text) = &self.last {
                        let _ = control.write_all(&set_clipboard(text));
                    }
                    self.devices.insert(name, control);
                }
                Event::Detach { name } => {
                    self.devices.remove(&name);
                }
            }
        }
        Ok(())
    }

    /// Gives `text` to every side except `source` (None is the desktop).
    fn share(&mut self, source: Option<&str>, text: String) {
        let from = source.unwrap_or("desktop");
        if self.last.as_ref() == Some(&text) {
            trace(|| format!("clipboard: {from} echoed the current text, ignored"));
            return;
        }
        trace(|| format!("clipboard: {from} copied {} bytes", text.len()));
        if source.is_some()
            && let Some(host) = &self.host
            && let Err(error) = host.set(&text)
        {
            eprintln!("could not set the desktop clipboard: {error:#}");
        }
        let message = set_clipboard(&text);
        self.devices.retain(|name, control| {
            Some(name.as_str()) == source || control.write_all(&message).is_ok()
        });
        self.last = Some(text);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::TcpListener;
    use std::time::Duration;

    use super::*;

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        server
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        (client, server)
    }

    fn received(device: &mut TcpStream) -> Option<String> {
        let mut header = [0u8; 14];
        device.read_exact(&mut header).ok()?;
        let length = u32::from_be_bytes(header[10..14].try_into().unwrap()) as usize;
        let mut text = vec![0u8; length];
        device.read_exact(&mut text).ok()?;
        Some(String::from_utf8(text).unwrap())
    }

    fn hub_with(names: &[&str]) -> (Hub, Vec<TcpStream>) {
        let mut hub = Hub::new(None);
        let mut devices = Vec::new();
        for name in names {
            let (control, device) = pair();
            hub.devices.insert(name.to_string(), control);
            devices.push(device);
        }
        (hub, devices)
    }

    #[test]
    fn desktop_copies_reach_every_device() {
        let (mut hub, mut devices) = hub_with(&["a", "b"]);
        hub.share(None, "hello".to_string());
        assert_eq!(received(&mut devices[0]).as_deref(), Some("hello"));
        assert_eq!(received(&mut devices[1]).as_deref(), Some("hello"));
    }

    #[test]
    fn a_device_copy_skips_its_source() {
        let (mut hub, mut devices) = hub_with(&["a", "b"]);
        hub.share(Some("a"), "from a".to_string());
        assert_eq!(received(&mut devices[0]), None);
        assert_eq!(received(&mut devices[1]).as_deref(), Some("from a"));
    }

    #[test]
    fn echoes_of_the_current_text_go_nowhere() {
        let (mut hub, mut devices) = hub_with(&["a", "b"]);
        hub.share(None, "same".to_string());
        received(&mut devices[0]);
        received(&mut devices[1]);
        hub.share(Some("a"), "same".to_string());
        assert_eq!(received(&mut devices[1]), None);
    }

    #[test]
    fn a_closed_device_is_dropped() {
        let (mut hub, devices) = hub_with(&["a"]);
        drop(devices);
        hub.share(None, "x".repeat(1 << 20));
        hub.share(None, "again".repeat(1 << 18));
        assert!(hub.devices.is_empty());
    }
}
