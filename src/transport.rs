use crate::config::PrinterConfig;
use anyhow::{Context, Result};
use rusb::{Context as UsbContext, DeviceHandle, Direction, TransferType, UsbContext as _};
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
    fn write_bytes(&mut self, data: &[u8]) -> Result<()>;
    fn query(
        &mut self,
        command: &[u8],
        first_byte: Duration,
        idle: Duration,
    ) -> Result<QueryResponse>;
    fn write_file(&mut self, path: &Path) -> Result<u64>;
}

pub fn open(config: &PrinterConfig) -> Result<Box<dyn PrinterTransport>> {
    match config.transport.as_str() {
        "char_device" => Ok(Box::new(CharDeviceTransport::open(config)?)),
        "usb_bulk" => Ok(Box::new(UsbBulkTransport::open(config)?)),
        value => anyhow::bail!("unsupported transport {value:?}"),
    }
}

pub struct CharDeviceTransport {
    device: File,
    write_timeout: Duration,
}

impl CharDeviceTransport {
    pub fn open(config: &PrinterConfig) -> Result<Self> {
        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&config.device)
            .with_context(|| format!("opening char device {}", config.device.display()))?;
        Ok(Self {
            device,
            write_timeout: Duration::from_millis(config.write_timeout_ms),
        })
    }
}

impl PrinterTransport for CharDeviceTransport {
    fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        write_all_nonblocking(&mut self.device, data, self.write_timeout)
    }
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

    fn write_file(&mut self, path: &Path) -> Result<u64> {
        let mut source = File::open(path)?;
        let mut total = 0;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = source.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            write_all_nonblocking(&mut self.device, &chunk[..n], self.write_timeout)?;
            total += n as u64;
        }
        Ok(total)
    }
}

pub struct UsbBulkTransport {
    handle: DeviceHandle<UsbContext>,
    interface: u8,
    in_endpoint: Option<u8>,
    out_endpoint: u8,
    write_timeout: Duration,
}

impl UsbBulkTransport {
    pub fn open(config: &PrinterConfig) -> Result<Self> {
        let vendor_id = config
            .usb_vendor_id
            .context("usb_bulk requires usb_vendor_id")?;
        let product_id = config
            .usb_product_id
            .context("usb_bulk requires usb_product_id")?;
        let usb = UsbContext::new().context("initializing libusb")?;
        let devices = usb.devices().context("enumerating USB devices")?;
        let mut matching = Vec::new();
        for device in devices.iter() {
            let descriptor = device
                .device_descriptor()
                .context("reading USB device descriptor")?;
            if descriptor.vendor_id() != vendor_id || descriptor.product_id() != product_id {
                continue;
            }
            if let Some(expected) = config.usb_serial.as_deref() {
                let handle = device.open().with_context(|| {
                    format!(
                        "opening USB device {:04x}:{:04x} to verify serial",
                        vendor_id, product_id
                    )
                })?;
                let actual = handle
                    .read_serial_number_string_ascii(&descriptor)
                    .context("reading USB serial number")?;
                if actual != expected {
                    continue;
                }
            }
            matching.push(device);
        }
        anyhow::ensure!(
            !matching.is_empty(),
            "USB device {:04x}:{:04x}{} not found",
            vendor_id,
            product_id,
            config
                .usb_serial
                .as_ref()
                .map(|_| " with configured serial".to_string())
                .unwrap_or_default()
        );
        anyhow::ensure!(
            matching.len() == 1,
            "multiple USB devices match {:04x}:{:04x}; configure usb_serial",
            vendor_id,
            product_id
        );
        let device = matching.pop().expect("one matching USB device");
        let descriptor = device
            .active_config_descriptor()
            .or_else(|_| device.config_descriptor(0))
            .context("reading active USB configuration")?;
        let mut selected = None;
        for interface in descriptor.interfaces() {
            for setting in interface.descriptors() {
                if setting.class_code() != 7 {
                    continue;
                }
                let mut bulk_in = None;
                let mut bulk_out = None;
                for endpoint in setting.endpoint_descriptors() {
                    if endpoint.transfer_type() != TransferType::Bulk {
                        continue;
                    }
                    match endpoint.direction() {
                        Direction::In => bulk_in = Some(endpoint.address()),
                        Direction::Out => bulk_out = Some(endpoint.address()),
                    }
                }
                if let Some(out_endpoint) = bulk_out {
                    selected = Some((
                        setting.interface_number(),
                        setting.setting_number(),
                        bulk_in,
                        out_endpoint,
                    ));
                    break;
                }
            }
            if selected.is_some() {
                break;
            }
        }
        let (interface, alternate_setting, in_endpoint, out_endpoint) =
            selected.context("USB device has no printer-class bulk OUT endpoint")?;
        let handle = device
            .open()
            .with_context(|| format!("opening USB printer {:04x}:{:04x}", vendor_id, product_id))?;
        let _ = handle.set_auto_detach_kernel_driver(true);
        handle
            .claim_interface(interface)
            .with_context(|| format!("claiming USB printer interface {interface}"))?;
        if alternate_setting != 0 {
            handle
                .set_alternate_setting(interface, alternate_setting)
                .with_context(|| {
                    format!(
                        "selecting USB printer interface {interface} alternate {alternate_setting}"
                    )
                })?;
        }
        Ok(Self {
            handle,
            interface,
            in_endpoint,
            out_endpoint,
            write_timeout: Duration::from_millis(config.write_timeout_ms)
                .max(Duration::from_millis(1)),
        })
    }

    fn write_all_bulk(&self, mut data: &[u8], timeout: Duration) -> Result<()> {
        let timeout = timeout.max(Duration::from_millis(1));
        while !data.is_empty() {
            let written = self
                .handle
                .write_bulk(self.out_endpoint, data, timeout)
                .context("writing USB printer bulk endpoint")?;
            anyhow::ensure!(written > 0, "USB printer accepted zero bytes");
            data = &data[written..];
        }
        Ok(())
    }
}

impl Drop for UsbBulkTransport {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.interface);
    }
}

impl PrinterTransport for UsbBulkTransport {
    fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.write_all_bulk(data, self.write_timeout)
    }

    fn query(
        &mut self,
        command: &[u8],
        first_byte: Duration,
        idle: Duration,
    ) -> Result<QueryResponse> {
        let endpoint = self
            .in_endpoint
            .context("USB printer does not expose a bulk IN endpoint")?;
        let started = Instant::now();
        self.write_all_bulk(command, first_byte)?;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        match self.handle.read_bulk(
            endpoint,
            &mut chunk,
            first_byte.max(Duration::from_millis(1)),
        ) {
            Ok(0) => anyhow::bail!("first_byte_timeout"),
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(rusb::Error::Timeout) => anyhow::bail!("first_byte_timeout"),
            Err(error) => return Err(error).context("reading USB printer bulk endpoint"),
        }
        loop {
            match self
                .handle
                .read_bulk(endpoint, &mut chunk, idle.max(Duration::from_millis(1)))
            {
                Ok(0) | Err(rusb::Error::Timeout) => break,
                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                Err(error) => return Err(error).context("reading USB printer bulk endpoint"),
            }
        }
        let classification = classify(&bytes);
        Ok(QueryResponse {
            bytes,
            duration: started.elapsed(),
            classification,
        })
    }

    fn write_file(&mut self, path: &Path) -> Result<u64> {
        let mut source = File::open(path)?;
        let mut total = 0;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let read = source.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            self.write_all_bulk(&chunk[..read], self.write_timeout)?;
            total += read as u64;
        }
        Ok(total)
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
        let mut transport = CharDeviceTransport {
            device: file,
            write_timeout: Duration::from_secs(1),
        };
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
