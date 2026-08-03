# zpl-agent

`zpl-agent` is a small, headless Linux service for FIFO-controlled Zebra ZPL II
printers exposed as character devices. It provides REST/JSON, Prometheus metrics,
and DNS-SD only—there is no UI, database, CUPS/IPP, port 9100, authentication, or
automatic retry.

Every configured printer is announced separately through DNS-SD and has its own
worker, online state, and status endpoint. With `auto_discover = true`, additional
Linux printer character devices matching `/dev/usb/lpN` are added at startup and
announced as `usb-lpN`. Explicit entries take precedence, so stable udev aliases
can be used for named production printers.

## Quick start on Debian

Release installation (run as root):

```sh
curl -fsSL https://github.com/Hartmannlight/ZebraTamer/releases/latest/download/install.sh | sudo sh
```

`sudo` must apply to `sh` on the right-hand side of the pipe. Applying it only
to `curl` does not give the installer permission to write to `/usr/local/bin`.

For the deployed Raspberry Pis, use a release profile. The installer selects
the correct binary from the release, verifies its SHA-256 checksum and needs no
Rust toolchain on the Pi:

```sh
# Raspberry Pi 2 Model B / Zebra T402
curl -fsSL https://github.com/Hartmannlight/ZebraTamer/releases/latest/download/install.sh | sudo sh -s -- --profile pi2

# Raspberry Pi Zero 2 W / Zebra LP 2824 Plus and the existing F16 workflow
curl -fsSL https://github.com/Hartmannlight/ZebraTamer/releases/latest/download/install.sh | sudo sh -s -- --profile zero2w
```

Push a tag such as `v0.2.1` to publish both `armv7-unknown-linux-gnueabihf`
and `aarch64-unknown-linux-gnu` binaries to GitHub Releases automatically.

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

Build only the native 32-bit ARM artifact for a Raspberry Pi 2 on Windows:

```powershell
./scripts/build-armv7.ps1
```

The script uses a native x86_64 Rust compiler plus an ARMv7 linker in Docker,
so it does not compile under QEMU emulation. It retains downloaded crates and
incremental build output in Docker volumes, writes the binary and its SHA-256
file to `dist/armv7-local`, and requires no Rust installation on either Windows
or the Pi. Copy the binary to the Pi and atomically replace
`/usr/local/bin/zpl-agent`; keep the service running natively under systemd.

Pass the device through only on a Linux Docker host, for example
`--device=/dev/usb/lp0`. Native systemd deployment is recommended on the
Raspberry Pi because it simplifies device permissions and mDNS.

Build an ARM64 artifact for a Raspberry Pi Zero 2 W running 64-bit Raspberry
Pi OS:

```powershell
./scripts/build-arm64.ps1
```

The result is `dist/arm64-local/zpl-agent`. See `deploy/pi-zero-2w/` for the
LP 2824 Plus configuration and the `kuche-pi` F16 integration.

## Grafana

An importable, host- and printer-independent dashboard is available at
[`grafana/zebra-printers.json`](grafana/zebra-printers.json). Together with the
`pi-init` textfile collector, the API and print endpoint can remain bound to
localhost while the central Prometheus receives system and Zebra metrics from
the Node Exporter on port `9100`.

The **Pi** and **Printer** variables are populated from the common `host` target
label and the `printer` metric label. Newly registered hosts and printers appear
without creating or copying a dashboard.

## API notes

All API results use the versioned envelope. Job bodies with
`Content-Type: application/zpl` are streamed to disk and hashed without retaining
the complete request in memory. List endpoints accept `cursor` and `limit` (up to
200). Printer I/O is serialized by one bounded worker per configured printer;
different printers operate concurrently. Metrics use cached snapshots and never
perform printer I/O.

`GET /v1/printers` lists every printer with its independent `online` and
`present` observations. `GET /v1/printers/{id}/status` returns only the transport
and structured live status; `GET /v1/printers/{id}/snapshot` additionally returns
identity, settings, diagnostics, memory, counters, and maintenance data.

### Persistent printer settings

`GET /v1/printers/{id}/settings` returns both the settings managed by the agent
and the most recently observed values from the printer. `PATCH` applies a partial
update; omitted fields keep their previously managed value. The supported fields
are deliberately allowlisted:

| Field | Range | Zebra command |
| --- | ---: | --- |
| `darkness` | 0.0 to 30.0 | `~SD` |
| `print_speed_ips` | 1.0 to 14.0 | `^PR` |
| `slew_speed_ips` | 1.0 to 14.0 | `^PR` |
| `backfeed_speed_ips` | 1.0 to 14.0 | `^PR` |
| `x_offset_dots` | -9999 to 9999 | `^LS` |
| `y_offset_dots` | -120 to 120 | `^LT` |
| `tear_off_dots` | -120 to 120 | `~TA` |

```sh
curl -X PATCH http://localhost:8080/v1/printers/schildkrote/settings \
  -H 'Content-Type: application/json' \
  -d '{"darkness":18.5,"print_speed_ips":4,"x_offset_dots":-3,"y_offset_dots":8}'
```

The agent serializes this operation with print jobs and stores the desired values
under the selected printer. It first sends `^JUR` to restore the last saved
configuration, preventing transient values from previous print formats from
becoming permanent. It then applies the allowlisted changes and sends `^JUS` so
the printer saves the result in nonvolatile memory. If bidirectional queries are
enabled, it reads the settings back. The response reports `verified`, `mismatch`,
or `applied_unverified` and includes individual mismatches.

Head temperature is diagnostic data, not a configurable target temperature.
For Zebra thermal printers, print energy is controlled with `darkness`. Media
tracking, calibration, print mode, print technology, and raw SGD/ZPL commands
are not exposed by this endpoint because they can change media handling or
enable hardware such as cutters and peelers.

The frequent poll sends only `~HS`. Startup and the slower capability poll use
`~HI`, `~HD`, `~HM`, `~HB`, `^HH`, selected `~HQ` commands, `^HZr`, and an SGD
odometer fallback. Large directory/XML dumps and `allcv` are deliberately not
polled. Optional commands are reported as supported only after a response; an
offline timeout remains `unavailable`, not `not_supported`.

Set `bidirectional_queries = false` for print-only USB-to-parallel paths that
become unstable after reverse-channel queries. The API then reports live status
fields as unavailable while presence, job delivery, queues, and metrics remain
usable. Raw job writes are nonblocking and honor `write_timeout_ms`, so a stalled
adapter cannot hold a printer worker forever. Before close, the agent waits for
`POLLOUT`, clears `O_NONBLOCK`, and performs the zero-length write required by
Linux `usblp`. This prevents close from cancelling the final pending USB URB and
marks a job `transport_accepted` only after the kernel transfer has drained.
