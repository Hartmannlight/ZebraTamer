# Native Raspberry Pi installation

This is the preferred ZebraTamer-only deployment for memory-constrained Debian
or Raspberry Pi OS systems. It installs one stripped architecture-matching
`zpl-agent` binary under systemd and does not require Docker.

Place these files in one directory on the Pi:

- `zpl-agent`, built for the Pi architecture;
- a reviewed `config.toml`, based on `config.usb.toml.example`;
- `zpl-agent.service`, `70-zpl-agent-device.rules`, and `install-local.sh`.

The release asset named
`zebratamer-native-aarch64-unknown-linux-gnu.tar.gz` already has this layout
for a 64-bit Raspberry Pi. Verify its adjacent SHA-256 file before unpacking.

Then run:

```sh
chmod +x install-local.sh
sudo ./install-local.sh
```

The installer creates a 256-bit API token at `/etc/zpl-agent/token`, validates
the configuration before replacement, retains the previous executable as
`/usr/local/bin/zpl-agent.previous`, grants the service account access through
the existing `lp` group, enables systemd restart handling, and performs a
bounded health check. Obtain the token locally on the Pi with
`sudo cat /etc/zpl-agent/token` and register `http://PI_ADDRESS:8080` in
PrintHub.

For automation, set `ZEBRATAMER_TOKEN_FILE` to a pre-generated token file. The
installer copies it with restricted permissions instead of creating a new
token, which lets the central PrintHub be configured without displaying the
secret in a terminal.

The checked-in udev rule is intentionally restricted to Zebra vendor ID
`0a5f`. For another vendor or a `usb_bulk` configuration, review the rule and
systemd group access before installation.
