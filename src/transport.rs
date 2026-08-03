use crate::config::PrinterConfig;
use anyhow::{Context, Result};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::fd::AsRawFd,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};

pub struct QueryResponse {
    pub bytes: Vec<u8>,
    pub duration: Duration,
    pub classification: String,
}

pub trait PrinterTransport: Send {
    fn query(
        &mut self,
        command: &[u8],
        first_byte: Duration,
        idle: Duration,
    ) -> Result<QueryResponse>;
}

pub struct CharDeviceTransport {
    device: File,
}

impl CharDeviceTransport {
    pub fn open(config: &PrinterConfig) -> Result<Self> {
        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&config.device)
            .with_context(|| format!("opening char device {}", config.device.display()))?;
        Ok(Self { device })
    }

    /// Send one raw job with a fresh write-only file descriptor.
    ///
    /// Querying needs a read/write descriptor, but USB-to-parallel bridges can
    /// be sensitive to one remaining open while a print job starts. Keeping
    /// the job path separate reproduces the established open/write/close raw
    /// delivery pattern while the agent remains the sole process with access.
    pub fn write_file(config: &PrinterConfig, path: &Path) -> Result<u64> {
        let mut output = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(&config.device)
            .with_context(|| format!("opening printer for write {}", config.device.display()))?;
        let mut source = File::open(path)?;
        let mut total = 0;
        let deadline = Instant::now() + config.write_timeout();
        let mut chunk = [0u8; 4 * 1024];
        loop {
            let n = source.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                anyhow::bail!("write_timeout");
            }
            write_all_nonblocking(&mut output, &chunk[..n], remaining)?;
            total += n as u64;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("write_drain_timeout");
        }
        drain_nonblocking_write(&output, remaining)?;
        Ok(total)
    }

    /// Send one small driver-generated command with the same drained,
    /// write-only delivery semantics as a print job.
    pub fn write_bytes(config: &PrinterConfig, bytes: &[u8]) -> Result<u64> {
        let mut output = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(&config.device)
            .with_context(|| format!("opening printer for write {}", config.device.display()))?;
        write_all_nonblocking(&mut output, bytes, config.write_timeout())?;
        drain_nonblocking_write(&output, config.write_timeout())?;
        Ok(bytes.len() as u64)
    }
}

impl PrinterTransport for CharDeviceTransport {
    fn query(
        &mut self,
        command: &[u8],
        first_byte: Duration,
        idle: Duration,
    ) -> Result<QueryResponse> {
        let started = Instant::now();
        write_all_nonblocking(&mut self.device, command, first_byte)?;
        let mut bytes = Vec::new();
        if !poll_fd(self.device.as_raw_fd(), libc::POLLIN, first_byte)? {
            anyhow::bail!("first_byte_timeout");
        }
        loop {
            let mut chunk = [0u8; 4096];
            match self.device.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
            if !poll_fd(self.device.as_raw_fd(), libc::POLLIN, idle)? {
                break;
            }
        }
        let classification = classify(&bytes);
        Ok(QueryResponse {
            bytes,
            duration: started.elapsed(),
            classification,
        })
    }
}

fn poll_fd(fd: i32, events: i16, timeout: Duration) -> Result<bool> {
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    let mut pfd = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut pfd, 1, ms) };
    if result < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(result > 0)
}

fn write_all_nonblocking(file: &mut File, mut data: &[u8], timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while !data.is_empty() {
        match file.write(data) {
            Ok(0) => anyhow::bail!("device accepted zero bytes"),
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() || !poll_fd(file.as_raw_fd(), libc::POLLOUT, remaining)? {
                    anyhow::bail!("write_timeout");
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Wait until an asynchronous usblp write has really left the kernel before
/// closing its file descriptor.
///
/// usblp deliberately lets an O_NONBLOCK write return while the final USB URB
/// is still pending. Its release handler kills pending URBs, so close(2) at
/// that point silently truncates the printer stream. The kernel documents two
/// valid drain mechanisms: poll for POLLOUT, or clear O_NONBLOCK and issue a
/// zero-length write. Use both so the close that follows cannot cancel data.
fn drain_nonblocking_write(file: &File, timeout: Duration) -> Result<()> {
    let fd = file.as_raw_fd();
    if !poll_fd(fd, libc::POLLOUT, timeout)? {
        anyhow::bail!("write_drain_timeout");
    }

    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("reading printer descriptor flags");
    }
    if flags & libc::O_NONBLOCK != 0 {
        let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
        if result < 0 {
            return Err(std::io::Error::last_os_error())
                .context("switching printer descriptor to blocking mode");
        }
    }

    let result = unsafe { libc::write(fd, std::ptr::null(), 0) };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("draining printer write");
    }
    Ok(())
}

fn classify(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    if text.trim().is_empty() {
        "empty"
    } else if text.contains("error") || text.contains("invalid") {
        "printer_error"
    } else if text.contains("zebra") || text.contains("firmware") || text.contains("memory") {
        "structured"
    } else {
        "response"
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::fd::{FromRawFd, IntoRawFd},
        os::unix::net::UnixStream,
        thread,
    };

    #[test]
    fn query_collects_delayed_chunks_until_idle_window() {
        let (client, mut printer) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        let file = unsafe { File::from_raw_fd(client.into_raw_fd()) };
        let mut transport = CharDeviceTransport { device: file };
        let simulator = thread::spawn(move || {
            let mut command = [0u8; 3];
            printer.read_exact(&mut command).unwrap();
            assert_eq!(&command, b"~HS");
            thread::sleep(Duration::from_millis(30));
            printer.write_all(b"first").unwrap();
            thread::sleep(Duration::from_millis(40));
            printer.write_all(b"second").unwrap();
            thread::sleep(Duration::from_millis(120));
        });
        let answer = transport
            .query(
                b"~HS",
                Duration::from_millis(200),
                Duration::from_millis(80),
            )
            .unwrap();
        simulator.join().unwrap();
        assert_eq!(answer.bytes, b"firstsecond");
        assert!(answer.duration >= Duration::from_millis(140));
    }

    #[test]
    fn nonblocking_write_honors_timeout() {
        let (client, _printer) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        let mut file = unsafe { File::from_raw_fd(client.into_raw_fd()) };
        let payload = vec![0u8; 4 * 1024 * 1024];
        let error = write_all_nonblocking(&mut file, &payload, Duration::from_millis(10))
            .expect_err("an unread socket must eventually stop accepting bytes");
        assert!(error.to_string().contains("write_timeout"));
    }

    fn fill_nonblocking_socket(file: &mut File) {
        let chunk = [0u8; 64 * 1024];
        loop {
            match file.write(&chunk) {
                Ok(0) => panic!("socket accepted zero bytes"),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(error) => panic!("filling socket failed: {error}"),
            }
        }
    }

    #[test]
    fn final_nonblocking_write_must_drain_before_close() {
        let (client, mut printer) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        let mut file = unsafe { File::from_raw_fd(client.into_raw_fd()) };
        fill_nonblocking_socket(&mut file);

        let reader = thread::spawn(move || {
            thread::sleep(Duration::from_millis(40));
            let mut received = vec![0u8; 1024 * 1024];
            let bytes = printer.read(&mut received).unwrap();
            // Keep the peer alive until the drain completes. Closing it here
            // races the zero-length write below and can produce EPIPE.
            (bytes, printer)
        });
        let started = Instant::now();
        drain_nonblocking_write(&file, Duration::from_secs(1)).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(30));
        let (bytes, _printer) = reader.join().unwrap();
        assert!(bytes > 0);

        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(flags & libc::O_NONBLOCK, 0);
    }

    #[test]
    fn final_nonblocking_write_drain_honors_timeout() {
        let (client, _printer) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        let mut file = unsafe { File::from_raw_fd(client.into_raw_fd()) };
        fill_nonblocking_socket(&mut file);

        let error = drain_nonblocking_write(&file, Duration::from_millis(10))
            .expect_err("a full socket must not report a completed drain");
        assert!(error.to_string().contains("write_drain_timeout"));
    }
}
