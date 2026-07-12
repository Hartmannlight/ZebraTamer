# Raspberry Pi 2 deployment

This deployment targets 32-bit Raspbian (`armhf`, `armv7l`) and opens the kernel
printer character device `/dev/usb/lp0` directly. Run `install-local.sh` as root
after placing the release binary and packaging files at the paths referenced by
the script.

Observed adapter during initial deployment:

- USB ID: `1a86:7584`
- Product: CH340S / `USB2.0-Print`
- Kernel driver: `usblp`
- Kernel device: `/dev/usb/lp0`

The adapter accepted writes, but returned no bytes for `~HS`, `~HD`, `~HM`, or
`^XA^HH^XZ` within five seconds. Consequently the deployed configuration uses a
1.5-second first-byte timeout, reports `protocol_up = false`, and disables
hardware counters. Printing remains available; status fields stay unavailable
until a working IEEE-1284 reverse channel is provided.

The udev rule only grants the `zpl-agent` service account access. It creates no
stable alias and installs no competing sender process. If multiple USB printer
adapters are connected, replace the direct `lp0` configuration with explicit
device discovery before production use.
