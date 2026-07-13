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
            .custom_flags(libc::O_NOCTTY)
            .open(&config.device)
            .with_context(|| format!("opening printer for write {}", config.device.display()))?;
        let mut source = File::open(path)?;
        let mut total = 0;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = source.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            output.write_all(&chunk[..n])?;
            total += n as u64;
        }
        Ok(total)
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
}
