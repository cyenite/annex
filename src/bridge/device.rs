use std::fs::File;
use std::io::Read;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::adb::Adb;
use crate::paths::log_dir;
use crate::process::log_tail;
use crate::scrcpy::Scrcpy;

const SHELL_UID: u32 = 2000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PUSH_TIMEOUT: Duration = Duration::from_secs(30);
// A separate name from scrcpy's own jar, so `annex show` pushing its copy never swaps the file
// under a running bridge.
const REMOTE_JAR: &str = "/data/local/tmp/annex-scrcpy-server.jar";

/// scrcpy's server jar and the exact version string it must be started with.
pub struct ServerJar {
    path: PathBuf,
    version: String,
}

impl ServerJar {
    pub fn locate() -> Result<Self> {
        let scrcpy = Scrcpy::locate()?;
        Ok(Self {
            path: scrcpy.server()?,
            version: scrcpy.version()?,
        })
    }
}

/// A headless scrcpy server on one instance: no video, no audio, only the control socket,
/// which reports the device clipboard and accepts new clipboard text.
pub struct Session {
    adb: Adb,
    port: u16,
    serial: String,
    server: Child,
}

impl Session {
    /// Starts the server and returns the session with the connected control socket.
    pub fn start(
        adb: &Adb,
        name: &str,
        serial: &str,
        jar: &ServerJar,
    ) -> Result<(Self, TcpStream)> {
        let jar_path = jar.path.to_string_lossy();
        adb.run(serial, &["push", &jar_path, REMOTE_JAR], PUSH_TIMEOUT)?;
        let scid = session_id();
        let port = adb.forward(serial, &format!("scrcpy_{scid:08x}"))?;
        let log = log_dir().join(format!("bridge-{name}.log"));
        std::fs::create_dir_all(log_dir())?;
        let server = [
            format!("CLASSPATH={REMOTE_JAR}"),
            "app_process / com.genymobile.scrcpy.Server".to_string(),
            jar.version.clone(),
            format!("scid={scid:08x}"),
            "log_level=warn video=false audio=false control=true".to_string(),
            "tunnel_forward=true send_device_meta=false send_dummy_byte=true".to_string(),
            "clipboard_autosync=true power_on=false cleanup=false".to_string(),
        ]
        .join(" ");
        // As root the server can still set the clipboard, but Android never reports clipboard
        // changes to uid 0 posing as the shell package. `annex open` leaves adbd as root.
        let command = if adb.is_root(serial) {
            format!("su {SHELL_UID} sh -c '{server}'")
        } else {
            server
        };
        let output = File::create(&log)?;
        let server = Command::new(adb.path())
            .args(["-s", serial, "shell", &command])
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output)
            .spawn()
            .context("failed to start the clipboard helper")?;
        let mut session = Self {
            adb: adb.clone(),
            port,
            serial: serial.to_string(),
            server,
        };
        let socket = session.connect().with_context(|| log_tail(&log, 6))?;
        Ok((session, socket))
    }

    /// adb accepts the local connection before the server listens and then closes it, so a
    /// connection only counts once the server's dummy byte arrives.
    fn connect(&mut self) -> Result<TcpStream> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.server.try_wait()? {
                bail!("the clipboard helper exited ({status})");
            }
            if let Ok(mut socket) = TcpStream::connect(("127.0.0.1", self.port)) {
                socket.set_read_timeout(Some(Duration::from_secs(1)))?;
                let mut dummy = [0u8; 1];
                if socket.read_exact(&mut dummy).is_ok() {
                    socket.set_read_timeout(None)?;
                    return Ok(socket);
                }
            }
            if started.elapsed() > CONNECT_TIMEOUT {
                bail!("the clipboard helper did not accept a connection");
            }
            sleep(Duration::from_millis(200));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
        self.adb.remove_forward(&self.serial, self.port);
    }
}

fn session_id() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    (nanos ^ std::process::id().rotate_left(16)) & 0x7fff_ffff
}
