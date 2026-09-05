# PrintAgent

PrintAgent is a small Linux edge service for locally attached printers. The
current `zpl-agent` binary and installation paths retain their historical names
for packaging compatibility while deployments migrate. Its first production
driver supports FIFO-controlled Zebra ZPL II printers exposed as character
devices. It provides REST/JSON, Prometheus metrics, DNS-SD,
and an optional built-in WebUI for persistent device settings and loaded media.
There is no database server, CUPS/IPP, port 9100 listener, or automatic retry.

PrinterFleet is the global source of truth and forwards immutable device
payloads with `X-Idempotency-Key`. Reusing the same key and payload returns the
original agent job; reusing it for another printer or payload returns HTTP 409.
The configured `driver = "zpl"` is explicit so a future Niimbot driver can add
its own encoder and USB/Bluetooth transport without changing Fleet or PrintHub.
`GET /v1/drivers` exposes active and reserved payload contracts. The
`niimbot_b1` slot is intentionally reported as unavailable until its framing,
compression and real-hardware behavior are implemented and tested.

## Optional WebUI and persistent printer settings

The WebUI is embedded in the binary: no Node.js, frontend server or PrintHub is
required. It is disabled by default. In `/etc/zpl-agent/config.toml`, set these
**top-level** values (before any `[[printers]]` table):

```toml
webui_enabled = true
admin_token = "your-own-random-token-at-least-24-characters"
```

Generate a private random token, for example with `openssl rand -hex 24`, protect
the config file, and restart `zpl-agent`. Open `http://<agent-host>:8080/ui/` and
enter that token. It is not embedded in the HTML or saved in browser storage.
The device APIs also work with the WebUI disabled when an admin token is set.

**Network boundary:** the legacy print/job API remains unauthenticated and can
send arbitrary ZPL. The admin token is not a security boundary against clients
that can access that API. Keep the whole service on a trusted network or behind
an authenticated TLS reverse proxy/firewall; do not expose port 8080 publicly.
The browser UI uses same-origin requests, no external assets and a restrictive CSP.

For each printer, configure `[printers.device_profile]` after its `[[printers]]`
entry. Set the actual DPI, limits, and installed `thermal_transfer`, `peel_off`,
and `cutter` options; see `config.example.toml`. These are operator-confirmed
hardware capabilities, **not auto-detected hardware claims**. Conservative generic
defaults disable those three options. Older firmware may not report every field;
unreadable/ambiguous fields stay disabled rather than being guessed.

### Device values vs. loaded media

- **Device values:** absolute darkness (`~SD`), speed (`^PR`), left/top position
  (`^LS`/`^LT`), print width (`^PW`), continuous-media length (`^LL`), output mode
  (`^MM`), direct-thermal/thermal-transfer (`^MT`), and media tracking (`^MN`).
- **Loaded media:** name, size, color, material/technology, and roll accounting
  belong to ZebraTamer, in `data_dir/printers/<id>/media.json`. Color is never
  sent as a printer setting. Editing metadata preserves consumption. Loading a
  new roll explicitly archives the old state and starts new accounting.
- For gap/mark labels, length is determined by the printer's media calibration.
  This UI does not automatically feed/calibrate the printer when metadata changes.
  Use the printer's calibration procedure when changing that stock. The stored
  physical label dimensions still drive clients' layouts independently.

"Im Drucker speichern" executes in the same per-printer worker as jobs and polls:
fresh `^HH` read → revision check → apply only changed typed settings → read back
and compare → send `^JUS` only if the requested fields match → read back again.
No job/poll can interleave. `^JUS` saves **all current persistent parameters**, not
just edited fields; inspect the raw configuration and acknowledge this before
saving. Other clients' prior ZPL may have changed those parameters.

The result distinguishes `not_saved`, `apply_outcome_unknown`,
`save_outcome_unknown`, and `save_sent_active_verified`. Successful transport is
not proof of flash persistence: the UI explicitly says power-cycle persistence
has not been verified. Failed/uncertain operations are never automatically retried,
and runtime changes are not silently rolled back. Read the printer again first.
The last observation and attempt are persisted separately in `configuration.json`
and `configuration-attempt.json`; stale cached observations carry a timestamp.

Ordinary print jobs **do not** replay device settings or `preferred_settings`.
They transmit caller-supplied ZPL. A client that explicitly sends conflicting ZPL
can still override the active settings. Updated PrintHub leaves these device
defaults alone; its old local printer values remain archival migration data only.
Set up the loaded roll and DPI in ZebraTamer before registering a new printer there.

### API

All responses retain the existing `v1` envelope. Administrative requests use
`Authorization: Bearer <admin_token>`.

| Endpoint | Meaning |
| --- | --- |
| `GET /v1/printers/{id}/configuration` | Cached observation, declared profile, last save, authoritative media + edit revision; no device I/O |
| `POST /v1/printers/{id}/configuration/read` | Read live device configuration; token required |
| `POST /v1/printers/{id}/configuration` | `{revision, settings, confirm_save_all: true}`; token required |
| `GET /v1/printers/{id}/media` | Existing authoritative media API |
| `PATCH /v1/printers/{id}/media` | `{revision, media}`; edit metadata without resetting counters |
| `PUT /v1/printers/{id}/media` | Explicitly load a new roll |

Media writes require the token whenever `admin_token` is configured. Existing
headless installations without a token retain their previous media-API behavior.
All media mutations are serialized with printing to prevent lost consumption.
Back up the **entire** `data_dir`, including agent identity and media history.

References: [Zebra configuration persistence](https://docs.zebra.com/us/en/printers/desktop/bm-zd620-and-zd420-desktop-printers-user-guide-ditamap/c-zd620-420-zpl-configuration/c-zd620-420-managing-the-zpl-printer-configuration.html),
[ZPL configuration commands](https://cpws.zebra.com/cpws/docs/gseries/GX_Darkness.pdf).

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
python3 tests/webui_integration.py target/release/zpl-agent
```

On Windows, the same Debian build can be reproduced with Docker:

```powershell
docker build --build-arg ZPL_AGENT_GIT_COMMIT=$(git rev-parse HEAD) -t zpl-agent .
```

Build only the native 32-bit ARM artifact for a Raspberry Pi 2 on Windows:

```powershell
./scripts/build-armv7.ps1
```

The script uses Docker Buildx with `linux/arm/v7`, writes the binary and its
SHA-256 file to `dist/armv7-local`, and requires no Rust installation on either
Windows or the Pi. Copy the binary to the Pi and atomically replace
`/usr/local/bin/zpl-agent`; keep the service running natively under systemd.

On a Linux Docker host with `usblp`, pass the character device through as
`--device=/dev/usb/lp0`. If that kernel module is unavailable, configure
`transport = "usb_bulk"` with the exact USB vendor ID, product ID and serial,
then pass the matching `/dev/bus/usb/<bus>/<device>` node. The agent discovers
the printer-class bulk endpoints from its USB descriptors and refuses an
ambiguous VID/PID match unless a serial is configured.

Docker Desktop does not provide direct host-USB passthrough. Attach the device
to its Linux VM with USB/IP first, following
[Docker's USB/IP guide](https://docs.docker.com/desktop/features/usbip/) or the
[Microsoft WSL usbipd-win guide](https://learn.microsoft.com/windows/wsl/connect-usb).
Give the agent's numeric group (999 in the published image) access only to that
device node; do not run PrintHub or PrinterFleet privileged. Native systemd
deployment remains recommended on a Raspberry Pi because it simplifies stable
udev permissions and mDNS.

## API notes

### Stable agent identity

The agent exposes `agent_id` in `GET /v1/agent` and both DNS-SD service TXT records.
Set an explicit, unique `agent_id = "workshop-pi"` in the configuration, or omit it
to generate a UUID on first normal startup. Generated IDs are persisted in
`data_dir/agent-id`; keep and back up that file across updates and IP changes.
Do not clone the same identity onto two physical agents. `--check-config` does
not create an identity or modify data. Invalid persisted identity fails startup
instead of silently generating a replacement. PrintHub uses the agent ID plus
local printer ID to avoid collisions between agents and preserve printer settings.

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


## Automated maintenance and releases

Push to main creates an immutable prerelease build-SHA-rRUN-ATTEMPT. Exact vMAJOR.MINOR.PATCH tags create immutable stable releases. No agent container is published. ARM64/AMD64 run native PTY integration tests on Debian Bookworm; ARMv7 is cross-compiled and is not runtime-tested.

See [policy](docs/SECURITY_RELEASE_POLICY.md), [required owner setup](docs/MANUAL_GITHUB_SETUP.md) and [rollback](docs/ROLLBACK.md).
Renovate auto-merge remains blocked until protected-branch checks are verified. No deployment automation is installed.
