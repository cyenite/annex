//! The desktop clipboard, through X11. GNOME's Wayland compositor offers no clipboard-watching
//! protocol (wl-paste --watch needs wlroots' data-control), but it mirrors the clipboard to
//! XWayland, where XFixes reports every change without polling or stealing focus.

use std::env;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, bail};
use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    Property, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

use super::hub::Event;
use super::protocol::{TEXT_MAX_BYTES, truncate};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        incr: &b"INCR"[..],
        text: b"TEXT",
        wake: b"ANNEX_WAKE",
        string: b"STRING",
        targets: b"TARGETS",
        transfer: b"ANNEX_CLIPBOARD",
        clipboard: b"CLIPBOARD",
        text_plain: b"text/plain;charset=utf-8",
        utf8_string: b"UTF8_STRING",
    }
}

/// A handle for replacing the desktop clipboard; the X11 thread owns the selection and serves it.
pub struct HostClipboard {
    atoms: Atoms,
    window: Window,
    pending: Arc<Mutex<Option<String>>>,
    connection: RustConnection,
}

impl HostClipboard {
    pub fn start(events: Sender<Event>) -> Result<Self> {
        if env::var_os("DISPLAY").is_none() {
            bail!("DISPLAY is not set, so there is no X11 or XWayland clipboard to sync with");
        }
        let (connection, screen) = x11rb::connect(None).context("could not reach the X server")?;
        let root = connection.setup().roots[screen].root;
        let atoms = Atoms::new(&connection)?.reply()?;
        let window = connection.generate_id()?;
        connection.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        connection
            .xfixes_query_version(5, 0)?
            .reply()
            .context("the X server has no XFixes extension")?;
        connection.xfixes_select_selection_input(
            window,
            atoms.clipboard,
            SelectionEventMask::SET_SELECTION_OWNER,
        )?;
        // Read what's already on the clipboard, so the first instance to attach receives it.
        connection.convert_selection(
            window,
            atoms.clipboard,
            atoms.utf8_string,
            atoms.transfer,
            CURRENT_TIME,
        )?;
        connection.flush()?;

        let pending = Arc::new(Mutex::new(None));
        let mut owner = Owner {
            atoms,
            window,
            incoming: None,
            owned: None,
            pending: pending.clone(),
            connection,
        };
        thread::Builder::new()
            .name("host-clipboard".to_string())
            .spawn(move || {
                let error = owner.run(&events).unwrap_err();
                let _ = events.send(Event::HostFailed(format!("{error:#}")));
            })?;

        let (connection, _) = x11rb::connect(None)?;
        Ok(Self {
            atoms,
            window,
            pending,
            connection,
        })
    }

    /// Takes over the desktop clipboard with `text`. Only the thread that owns the selection
    /// can answer paste requests, so this hands the text over and wakes it with a client message.
    pub fn set(&self, text: &str) -> Result<()> {
        *self.pending.lock().expect("clipboard lock") =
            Some(truncate(text, TEXT_MAX_BYTES).to_string());
        let wake = ClientMessageEvent::new(32, self.window, self.atoms.wake, [0u32; 5]);
        self.connection
            .send_event(false, self.window, EventMask::NO_EVENT, wake)?;
        self.connection.flush()?;
        Ok(())
    }
}

struct Owner {
    atoms: Atoms,
    window: Window,
    /// Text arriving in chunks through the INCR protocol, when a paste is too large for one property.
    incoming: Option<Vec<u8>>,
    owned: Option<String>,
    pending: Arc<Mutex<Option<String>>>,
    connection: RustConnection,
}

impl Owner {
    fn run(&mut self, events: &Sender<Event>) -> Result<()> {
        loop {
            match self.connection.wait_for_event()? {
                XEvent::XfixesSelectionNotify(event) => {
                    if event.owner != self.window && event.owner != NONE {
                        self.connection.convert_selection(
                            self.window,
                            self.atoms.clipboard,
                            self.atoms.utf8_string,
                            self.atoms.transfer,
                            event.timestamp,
                        )?;
                    }
                }
                XEvent::SelectionNotify(event) if event.property != NONE => {
                    let reply = self.take_transfer()?;
                    if reply.type_ == self.atoms.incr {
                        self.incoming = Some(Vec::new());
                    } else {
                        send_text(events, &reply.value)?;
                    }
                }
                XEvent::PropertyNotify(event)
                    if event.atom == self.atoms.transfer
                        && event.state == Property::NEW_VALUE
                        && self.incoming.is_some() =>
                {
                    let reply = self.take_transfer()?;
                    let incoming = self.incoming.as_mut().expect("checked above");
                    if reply.value.is_empty() {
                        let text = self.incoming.take().unwrap_or_default();
                        send_text(events, &text)?;
                    } else if incoming.len() < TEXT_MAX_BYTES {
                        incoming.extend_from_slice(&reply.value);
                    }
                }
                XEvent::SelectionRequest(request) => self.answer(&request)?,
                XEvent::SelectionClear(event) if event.selection == self.atoms.clipboard => {
                    self.owned = None;
                }
                XEvent::ClientMessage(message) if message.type_ == self.atoms.wake => {
                    let text = self.pending.lock().expect("clipboard lock").take();
                    if let Some(text) = text {
                        self.owned = Some(text);
                        self.connection.set_selection_owner(
                            self.window,
                            self.atoms.clipboard,
                            CURRENT_TIME,
                        )?;
                    }
                }
                _ => {}
            }
            self.connection.flush()?;
        }
    }

    fn take_transfer(&self) -> Result<x11rb::protocol::xproto::GetPropertyReply> {
        Ok(self
            .connection
            .get_property(
                true,
                self.window,
                self.atoms.transfer,
                AtomEnum::ANY,
                0,
                u32::MAX / 4,
            )?
            .reply()?)
    }

    fn answer(&self, request: &SelectionRequestEvent) -> Result<()> {
        let atoms = &self.atoms;
        let text_targets = [
            atoms.utf8_string,
            atoms.text_plain,
            atoms.string,
            atoms.text,
        ];
        // Pre-ICCCM clients leave the property empty and expect the target's name instead.
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let mut answered = NONE;
        if let Some(text) = &self.owned {
            if request.target == atoms.targets {
                let mut targets = vec![atoms.targets];
                targets.extend(text_targets);
                self.connection.change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &targets,
                )?;
                answered = property;
            } else if text_targets.contains(&request.target) {
                let kind = if request.target == atoms.text {
                    atoms.utf8_string
                } else {
                    request.target
                };
                self.connection.change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    kind,
                    text.as_bytes(),
                )?;
                answered = property;
            }
        }
        let notify = SelectionNotifyEvent {
            response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: answered,
        };
        self.connection
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify)?;
        Ok(())
    }
}

fn send_text(events: &Sender<Event>, bytes: &[u8]) -> Result<()> {
    let text = String::from_utf8_lossy(bytes);
    let text = truncate(&text, TEXT_MAX_BYTES);
    if !text.is_empty() {
        events
            .send(Event::Host(text.to_string()))
            .context("the clipboard hub stopped")?;
    }
    Ok(())
}
