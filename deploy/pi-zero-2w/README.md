# Raspberry Pi Zero 2 W deployment

The Zero 2 W is running 64-bit Raspberry Pi OS (`aarch64`). Build the agent on
Windows with `./scripts/build-arm64.ps1`. This deployment owns `/dev/usb/lp0`
for the Zebra LP 2824 Plus as printer ID `ente` and listens only on
`127.0.0.1:8080`.

`print-ente-label.py` is a replacement for the executable in the `kuche-pi`
repository. It preserves F16's dated-label behavior, but sends its two ZPL
jobs to the local agent API instead of opening `/dev/zpl/ente`.
