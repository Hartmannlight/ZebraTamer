# zpl-agent

`zpl-agent` is a small, headless Linux service for FIFO-controlled Zebra ZPL II
printers exposed as character devices. It provides REST/JSON, Prometheus metrics,
and DNS-SD only—there is no UI, database, CUPS/IPP, port 9100, authentication, or
automatic retry.

## Quick start on Debian

Release installation (run as root):

```sh
curl -fsSL https://github.com/Hartmannlight/ZebraTamer/releases/latest/download/install.sh | sh
```

Review `/etc/zpl-agent/config.toml` and the udev rule afterward. The example opens
the kernel character device `/dev/usb/lp0` directly. The packaged udev rule only
grants the dedicated service account access; it creates no alias and installs no
competing sender process.

## Build and test

```sh
cargo fmt --check
cargo test --locked
cargo build --release --locked
```

On Windows, the same Debian build can be reproduced with Docker:

```powershell
docker build --build-arg ZPL_AGENT_GIT_COMMIT=$(git rev-parse HEAD) -t zpl-agent .
```

Pass the device through only on a Linux Docker host, for example
`--device=/dev/usb/lp0`. Native systemd deployment is recommended on the
Raspberry Pi because it simplifies device permissions and mDNS.

## API notes

All API results use the versioned envelope. Job bodies with
`Content-Type: application/zpl` are streamed to disk and hashed without retaining
the complete request in memory. List endpoints accept `cursor` and `limit` (up to
200). Printer I/O is serialized by one bounded worker per configured printer;
different printers operate concurrently. Metrics use cached snapshots and never
perform printer I/O.

Probe commands include the T402-compatible `~HS`, `~HD`, `~HM`, `~HB`, `^HH`, and
the R/E/B/Z directory queries. Optional SGD, `~HQ`, and odometer commands are
reported as supported only after a response; an offline timeout remains
`unavailable`, not `not_supported`.
