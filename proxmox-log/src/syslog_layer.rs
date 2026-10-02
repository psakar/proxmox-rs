//! Logging to the system log via the syslog socket, for systems without journald.
//!
//! The messages are sent to the socket directly in the traditional (RFC 3164) format, rather
//! than via libc's `openlog`/`syslog`: their identifier is process-wide state, shared with
//! whatever else logs in the same process, e.g. Perl code in the PVE daemons.

use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing::{Level, Metadata};
use tracing_subscriber::fmt::MakeWriter;

/// The system log's socket.
pub const SYSLOG_SOCKET: &str = "/dev/log";

/// LOG_DAEMON, see syslog(3)
const FACILITY_DAEMON: u8 = 3;

fn severity(level: &Level) -> u8 {
    match *level {
        Level::ERROR => 3,                // LOG_ERR
        Level::WARN => 4,                 // LOG_WARNING
        Level::INFO => 6,                 // LOG_INFO
        Level::DEBUG | Level::TRACE => 7, // LOG_DEBUG
    }
}

struct Socket {
    path: PathBuf,
    socket: Mutex<UnixDatagram>,
    identifier: String,
}

impl Socket {
    fn connect(path: &Path) -> io::Result<UnixDatagram> {
        let socket = UnixDatagram::unbound()?;
        socket.connect(path)?;
        Ok(socket)
    }

    fn send(&self, severity: u8, message: &str) {
        let timestamp = proxmox_time::strftime_local("%b %e %H:%M:%S", proxmox_time::epoch_i64())
            .unwrap_or_default();
        let datagram = format!(
            "<{}>{timestamp} {}[{}]: {message}",
            FACILITY_DAEMON * 8 + severity,
            self.identifier,
            std::process::id(),
        );

        let mut socket = self.socket.lock().unwrap();
        if socket.send(datagram.as_bytes()).is_err() {
            // e.g. the syslog daemon was restarted: reconnect once, like libc does
            if let Ok(new_socket) = Self::connect(&self.path) {
                *socket = new_socket;
                let _ = socket.send(datagram.as_bytes());
            }
        }
    }
}

/// A [`MakeWriter`] sending each line written to it as a syslog message, with the severity of
/// the event's level, for use with a [`tracing_subscriber::fmt::Layer`].
#[derive(Clone)]
pub struct SyslogMakeWriter {
    socket: Arc<Socket>,
}

impl SyslogMakeWriter {
    /// Connect to the syslog socket at `path`. Messages are logged with `identifier`, like the
    /// identifier passed to syslog's `openlog`.
    pub fn new(path: &Path, identifier: String) -> io::Result<Self> {
        Ok(Self {
            socket: Arc::new(Socket {
                path: path.to_owned(),
                socket: Mutex::new(Socket::connect(path)?),
                identifier,
            }),
        })
    }
}

/// Buffers what's written for one event and sends it line by line when dropped.
pub struct SyslogWriter {
    socket: Arc<Socket>,
    severity: u8,
    buffer: Vec<u8>,
}

impl io::Write for SyslogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for SyslogWriter {
    fn drop(&mut self) {
        let text = String::from_utf8_lossy(&self.buffer);
        for line in text.lines().filter(|line| !line.is_empty()) {
            self.socket.send(self.severity, line);
        }
    }
}

impl SyslogMakeWriter {
    fn writer(&self, severity: u8) -> SyslogWriter {
        SyslogWriter {
            socket: Arc::clone(&self.socket),
            severity,
            buffer: Vec::new(),
        }
    }
}

impl<'a> MakeWriter<'a> for SyslogMakeWriter {
    type Writer = SyslogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.writer(severity(&Level::INFO))
    }

    fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Self::Writer {
        self.writer(severity(meta.level()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tracing_subscriber::prelude::*;

    #[test]
    fn sends_lines_with_severity() {
        let dir =
            std::env::temp_dir().join(format!("proxmox-log-syslog-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log");
        let server = UnixDatagram::bind(&path).unwrap();

        let writer = SyslogMakeWriter::new(&path, "test-ident".to_string()).unwrap();
        let layer = tracing_subscriber::fmt::layer()
            .event_format(
                tracing_subscriber::fmt::format()
                    .with_level(false)
                    .without_time()
                    .with_target(false)
                    .compact(),
            )
            .with_writer(writer);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("first message");
            tracing::error!("broken\nin two");
        });

        let mut received = Vec::new();
        let mut buf = [0u8; 1024];
        for _ in 0..3 {
            let len = server.recv(&mut buf).unwrap();
            received.push(String::from_utf8_lossy(&buf[..len]).into_owned());
        }
        std::fs::remove_dir_all(&dir).unwrap();

        let pid = std::process::id();
        let expect = |pri: u8, message: &str, got: &str| {
            assert!(got.starts_with(&format!("<{pri}>")), "priority in {got:?}");
            assert!(
                got.ends_with(&format!(" test-ident[{pid}]: {message}")),
                "identifier and message in {got:?}"
            );
        };
        expect(30, "first message", &received[0]); // daemon.info
        expect(27, "broken", &received[1]); // daemon.err
        expect(27, "in two", &received[2]);
    }

    #[test]
    fn missing_socket() {
        let path = Path::new("/nonexistent/proxmox-log-test/log");
        assert!(SyslogMakeWriter::new(path, "x".to_string()).is_err());
    }
}
