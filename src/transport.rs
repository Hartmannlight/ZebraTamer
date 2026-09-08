use crate::config::PrinterConfig;
use anyhow::{Context, Result};
use rusb::{Context as UsbContext, DeviceHandle, Direction, TransferType, UsbContext as _};
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs},
    os::fd::AsRawFd,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct QueryResponse {
    pub bytes: Vec<u8>,
    pub duration: Duration,
    pub classification: String,
}

#[derive(Debug)]
pub struct DeliveryFailure {
    pub bytes_written: u64,
    pub error: anyhow::Error,
}

impl std::fmt::Display for DeliveryFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.error)
    }
}

pub trait PrinterTransport: Send {
    fn write_bytes(&mut self, data: &[u8]) -> Result<()>;
    fn query(
        &mut self,
        command: &[u8],
        first_byte: Duration,
        idle: Duration,
    ) -> Result<QueryResponse>;
    fn write_file(&mut self, path: &Path) -> std::result::Result<u64, DeliveryFailure>;
}

pub fn discover_usb_printers() -> Result<Vec<Value>> {
    let usb = UsbContext::new().context("initializing libusb")?;
    let devices = usb.devices().context("enumerating USB devices")?;
    let mut found = Vec::new();
    for device in devices.iter() {
        let descriptor = match device.device_descriptor() {
            Ok(value) => value,
            Err(_) => continue,
        };
        let mut printer_class = descriptor.class_code() == 7;
        for index in 0..descriptor.num_configurations() {
            let Ok(configuration) = device.config_descriptor(index) else {
                continue;
            };
            if configuration.interfaces().any(|interface| {
                interface
                    .descriptors()
                    .any(|setting| setting.class_code() == 7)
            }) {
                printer_class = true;
                break;
            }
        }
        if !printer_class {
            continue;
        }
        let (manufacturer, product, serial_number) = match device.open() {
            Ok(handle) => (
                handle.read_manufacturer_string_ascii(&descriptor).ok(),
                handle.read_product_string_ascii(&descriptor).ok(),
                handle.read_serial_number_string_ascii(&descriptor).ok(),
            ),
            Err(_) => (None, None, None),
        };
        found.push(json!({
            "vendor_id": descriptor.vendor_id(),
            "product_id": descriptor.product_id(),
            "bus_number": device.bus_number(),
            "address": device.address(),
            "manufacturer": manufacturer,
            "product": product,
            "serial_number": serial_number,
            "stable_identity": serial_number.is_some()
        }));
    }
    Ok(found)
}

pub fn open(config: &PrinterConfig) -> Result<Box<dyn PrinterTransport>> {
    match config.transport.as_str() {
        "char_device" => Ok(Box::new(CharDeviceTransport::open(config)?)),
        "usb_bulk" => Ok(Box::new(UsbBulkTransport::open(config)?)),
        "tcp" => Ok(Box::new(TcpTransport::open(config)?)),
        value => anyhow::bail!("unsupported transport {value:?}"),
    }
}

pub struct TcpTransport {
    addresses: Vec<SocketAddr>,
    stream: Option<TcpStream>,
    wrote_since_connect: bool,
    connect_timeout: Duration,
    write_timeout: Duration,
    max_response_bytes: usize,
}

impl TcpTransport {
    pub fn open(config: &PrinterConfig) -> Result<Self> {
        let host = config
            .tcp_host
            .as_deref()
            .context("tcp transport requires tcp_host")?
            .trim();
        let addresses: Vec<_> = (host, config.tcp_port)
            .to_socket_addrs()
            .with_context(|| format!("resolving TCP printer {host}:{}", config.tcp_port))?
            .collect();
        anyhow::ensure!(
            !addresses.is_empty(),
            "TCP printer {host}:{} resolved to no addresses",
            config.tcp_port
        );
        let mut transport = Self {
            addresses,
            stream: None,
            wrote_since_connect: false,
            connect_timeout: Duration::from_millis(config.connect_timeout_ms),
            write_timeout: Duration::from_millis(config.write_timeout_ms),
            max_response_bytes: config.max_response_bytes,
        };
        transport.connect()?;
        Ok(transport)
    }

    fn connect(&mut self) -> Result<()> {
        let mut last_error = None;
        for address in &self.addresses {
            match TcpStream::connect_timeout(address, self.connect_timeout) {
                Ok(stream) => {
                    stream
                        .set_nodelay(true)
                        .context("configuring TCP printer socket")?;
                    stream
                        .set_write_timeout(Some(self.write_timeout))
                        .context("configuring TCP printer write timeout")?;
                    self.stream = Some(stream);
                    self.wrote_since_connect = false;
                    return Ok(());
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error
            .map(anyhow::Error::from)
            .unwrap_or_else(|| anyhow::anyhow!("TCP printer has no resolved address")))
        .context("connecting to TCP printer")
    }

    fn stream(&mut self) -> Result<&mut TcpStream> {
        if self.stream.is_none() {
            self.connect()?;
        }
        Ok(self.stream.as_mut().expect("stream connected"))
    }

    fn fresh_query_stream(&mut self) -> Result<TcpStream> {
        if self.stream.is_none() {
            self.connect()?;
        }
        if self.wrote_since_connect {
            if let Some(old) = self.stream.take() {
                let _ = old.shutdown(Shutdown::Both);
            }
            self.connect()?;
        }
        Ok(self.stream.take().expect("stream connected"))
    }
}

impl PrinterTransport for TcpTransport {
    fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.stream()?
            .write_all(data)
            .context("writing TCP printer")?;
        self.wrote_since_connect = true;
        Ok(())
    }

    fn query(
        &mut self,
        command: &[u8],
        first_byte: Duration,
        idle: Duration,
    ) -> Result<QueryResponse> {
        let started = Instant::now();
        let mut stream = self.fresh_query_stream()?;
        stream.set_write_timeout(Some(first_byte.max(Duration::from_millis(1))))?;
        stream
            .write_all(command)
            .context("writing TCP printer query")?;
        stream.set_read_timeout(Some(first_byte.max(Duration::from_millis(1))))?;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) if bytes.is_empty() => anyhow::bail!("connection_closed_before_response"),
                Ok(0) => break,
                Ok(n) => {
                    anyhow::ensure!(
                        bytes.len().saturating_add(n) <= self.max_response_bytes,
                        "response_too_large"
                    );
                    bytes.extend_from_slice(&chunk[..n]);
                    stream.set_read_timeout(Some(idle.max(Duration::from_millis(1))))?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) && bytes.is_empty() =>
                {
                    anyhow::bail!("first_byte_timeout")
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break
                }
                Err(error) => return Err(error).context("reading TCP printer response"),
            }
        }
        let _ = stream.shutdown(Shutdown::Both);
        let classification = classify(&bytes);
        Ok(QueryResponse {
            bytes,
            duration: started.elapsed(),
            classification,
        })
    }

    fn write_file(&mut self, path: &Path) -> std::result::Result<u64, DeliveryFailure> {
        let mut source = File::open(path).map_err(|error| DeliveryFailure {
            bytes_written: 0,
            error: error.into(),
        })?;
        let mut total = 0;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let read = source.read(&mut chunk).map_err(|error| DeliveryFailure {
                bytes_written: total,
                error: error.into(),
            })?;
            if read == 0 {
                break;
            }
            let stream = self.stream().map_err(|error| DeliveryFailure {
                bytes_written: total,
                error,
            })?;
            write_counted(stream, &chunk[..read], &mut total).map_err(|error| DeliveryFailure {
                bytes_written: total,
                error: error.context("writing TCP printer"),
            })?;
            self.wrote_since_connect = true;
        }
        Ok(total)
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

    fn write_file(&mut self, path: &Path) -> std::result::Result<u64, DeliveryFailure> {
        let mut source = File::open(path).map_err(|error| DeliveryFailure {
            bytes_written: 0,
            error: error.into(),
        })?;
        let mut total = 0;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = source.read(&mut chunk).map_err(|error| DeliveryFailure {
                bytes_written: total,
                error: error.into(),
            })?;
            if n == 0 {
                break;
            }
            write_all_nonblocking_counted(
                &mut self.device,
                &chunk[..n],
                self.write_timeout,
                &mut total,
            )
            .map_err(|error| DeliveryFailure {
                bytes_written: total,
                error,
            })?;
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

    fn write_file(&mut self, path: &Path) -> std::result::Result<u64, DeliveryFailure> {
        let mut source = File::open(path).map_err(|error| DeliveryFailure {
            bytes_written: 0,
            error: error.into(),
        })?;
        let mut total = 0;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let read = source.read(&mut chunk).map_err(|error| DeliveryFailure {
                bytes_written: total,
                error: error.into(),
            })?;
            if read == 0 {
                break;
            }
            let mut remaining = &chunk[..read];
            while !remaining.is_empty() {
                let written = self
                    .handle
                    .write_bulk(self.out_endpoint, remaining, self.write_timeout)
                    .map_err(|error| DeliveryFailure {
                        bytes_written: total,
                        error: anyhow::Error::from(error)
                            .context("writing USB printer bulk endpoint"),
                    })?;
                if written == 0 {
                    return Err(DeliveryFailure {
                        bytes_written: total,
                        error: anyhow::anyhow!("USB printer accepted zero bytes"),
                    });
                }
                total += written as u64;
                remaining = &remaining[written..];
            }
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

fn write_all_nonblocking_counted(
    file: &mut File,
    mut data: &[u8],
    timeout: Duration,
    total: &mut u64,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while !data.is_empty() {
        match file.write(data) {
            Ok(0) => anyhow::bail!("device accepted zero bytes"),
            Ok(n) => {
                *total += n as u64;
                data = &data[n..];
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() || !poll_fd(file.as_raw_fd(), libc::POLLOUT, remaining)? {
                    anyhow::bail!("write_timeout");
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn write_counted(writer: &mut impl Write, mut data: &[u8], total: &mut u64) -> Result<()> {
    while !data.is_empty() {
        let written = writer.write(data)?;
        anyhow::ensure!(written > 0, "printer accepted zero bytes");
        *total += written as u64;
        data = &data[written..];
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
        net::TcpListener,
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

    fn tcp_config(address: SocketAddr) -> PrinterConfig {
        PrinterConfig {
            id: "network-zebra".into(),
            transport: "tcp".into(),
            tcp_host: Some(address.ip().to_string()),
            tcp_port: address.port(),
            connect_timeout_ms: 500,
            first_byte_timeout_ms: 500,
            idle_timeout_ms: 80,
            write_timeout_ms: 500,
            max_response_bytes: 64,
            ..PrinterConfig::default()
        }
    }

    #[test]
    fn tcp_transport_writes_an_entire_job() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            stream.read_to_end(&mut received).unwrap();
            received
        });
        let mut transport = TcpTransport::open(&tcp_config(address)).unwrap();
        transport.write_bytes(b"^XA^FO1,1^FDtest^FS^XZ").unwrap();
        transport
            .stream
            .take()
            .unwrap()
            .shutdown(Shutdown::Both)
            .unwrap();
        assert_eq!(server.join().unwrap(), b"^XA^FO1,1^FDtest^FS^XZ");
    }

    #[test]
    fn tcp_query_collects_chunks_and_closes_the_query_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut command = [0u8; 3];
            stream.read_exact(&mut command).unwrap();
            assert_eq!(&command, b"~HS");
            thread::sleep(Duration::from_millis(20));
            stream.write_all(b"first").unwrap();
            thread::sleep(Duration::from_millis(20));
            stream.write_all(b"second").unwrap();
            thread::sleep(Duration::from_millis(120));
        });
        let mut transport = TcpTransport::open(&tcp_config(address)).unwrap();
        let response = transport
            .query(
                b"~HS",
                Duration::from_millis(200),
                Duration::from_millis(60),
            )
            .unwrap();
        server.join().unwrap();
        assert_eq!(response.bytes, b"firstsecond");
        assert!(transport.stream.is_none());
    }

    #[test]
    fn tcp_query_rejects_an_oversize_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut command = [0u8; 3];
            stream.read_exact(&mut command).unwrap();
            stream.write_all(&[b'x'; 65]).unwrap();
        });
        let mut transport = TcpTransport::open(&tcp_config(address)).unwrap();
        let error = transport
            .query(
                b"~HS",
                Duration::from_millis(200),
                Duration::from_millis(60),
            )
            .unwrap_err();
        server.join().unwrap();
        assert!(error.to_string().contains("response_too_large"));
    }

    #[test]
    fn tcp_query_reconnects_for_each_response_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for expected in [b"~HS", b"~HI"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut command = [0u8; 3];
                stream.read_exact(&mut command).unwrap();
                assert_eq!(&command, expected);
                stream.write_all(b"ok").unwrap();
            }
        });
        let mut transport = TcpTransport::open(&tcp_config(address)).unwrap();
        for command in [b"~HS", b"~HI"] {
            let response = transport
                .query(
                    command,
                    Duration::from_millis(200),
                    Duration::from_millis(30),
                )
                .unwrap();
            assert_eq!(response.bytes, b"ok");
        }
        server.join().unwrap();
    }
}
