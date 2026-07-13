#!/bin/sh
set -eu

REPOSITORY="${ZPL_AGENT_REPOSITORY:-Hartmannlight/ZebraTamer}"
VERSION="${ZPL_AGENT_VERSION:-latest}"
PROFILE="${ZPL_AGENT_PROFILE:-}"
BASE="https://github.com/${REPOSITORY}/releases"

usage() {
  cat <<'EOF'
Usage: install.sh [--profile pi2|zero2w]

Profiles:
  pi2     Raspberry Pi 2 Model B / Zebra T402 (32-bit ARMv7)
  zero2w  Raspberry Pi Zero 2 W / Zebra LP 2824 Plus (32- or 64-bit OS)

Set ZPL_AGENT_VERSION=vX.Y.Z to install a specific release. Without a profile,
the generic example configuration is installed for backward compatibility.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --profile)
      [ "$#" -ge 2 ] || { usage >&2; exit 2; }
      PROFILE="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$(uname -m)" in
  x86_64) target="x86_64-unknown-linux-gnu" ;;
  aarch64|arm64) target="aarch64-unknown-linux-gnu" ;;
  armv7l|armv7*) target="armv7-unknown-linux-gnueabihf" ;;
  *) echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

if [ "$VERSION" = latest ]; then
  url="$BASE/latest/download"
else
  url="$BASE/download/$VERSION"
fi

config_asset="config.example.toml"
expected_target=""
case "$PROFILE" in
  "") ;;
  pi2)
    config_asset="config-pi2.toml"
    expected_target="armv7-unknown-linux-gnueabihf"
    ;;
  zero2w)
    config_asset="config-zero2w.toml"
    ;;
  *)
    echo "Unsupported profile: $PROFILE" >&2
    usage >&2
    exit 2
    ;;
esac

if [ -n "$expected_target" ] && [ "$target" != "$expected_target" ]; then
  echo "Profile $PROFILE requires $expected_target, detected $target" >&2
  exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
asset="zpl-agent-$target"
curl -fsSL "$url/$asset" -o "$tmp/$asset"
curl -fsSL "$url/$asset.sha256" -o "$tmp/zpl-agent.sha256"
(cd "$tmp" && sha256sum -c zpl-agent.sha256)

install -m 0755 "$tmp/$asset" /usr/local/bin/zpl-agent
getent group zpl-agent >/dev/null 2>&1 || groupadd --system zpl-agent
id zpl-agent >/dev/null 2>&1 || useradd --system --gid zpl-agent --home-dir /var/lib/zpl-agent --shell /usr/sbin/nologin zpl-agent
install -d -o zpl-agent -g zpl-agent -m 0750 /var/lib/zpl-agent
install -d -o root -g zpl-agent -m 0750 /etc/zpl-agent

if [ ! -e /etc/zpl-agent/config.toml ]; then
  curl -fsSL "$url/$config_asset" -o /etc/zpl-agent/config.toml
  chown root:zpl-agent /etc/zpl-agent/config.toml
  chmod 0640 /etc/zpl-agent/config.toml
fi
curl -fsSL "$url/zpl-agent.service" -o /etc/systemd/system/zpl-agent.service
curl -fsSL "$url/70-zpl-agent-device.rules" -o /etc/udev/rules.d/70-zpl-agent-device.rules
curl -fsSL "$url/zpl-agent.modules.conf" -o /etc/modules-load.d/zpl-agent.conf

case "$PROFILE" in
  pi2)
    rm -f /etc/udev/rules.d/70-zpl-usb-parallel-schildkrote.rules
    ;;
  zero2w)
    rm -f /etc/udev/rules.d/70-zpl-usb-parallel-ente.rules
    curl -fsSL "$url/print-ente-label.py" -o "$tmp/print-ente-label.py"
    if [ -d /usr/local/lib/kuche-pi-audio ]; then
      install -m 0755 "$tmp/print-ente-label.py" /usr/local/lib/kuche-pi-audio/print-ente-label.py
    fi
    if [ -e /etc/systemd/system/audio-buttons.service ]; then
      install -d -m 0755 /etc/systemd/system/audio-buttons.service.d
      curl -fsSL "$url/audio-buttons-zpl-agent.conf" -o /etc/systemd/system/audio-buttons.service.d/zpl-agent.conf
    fi
    ;;
esac
rm -f /etc/tmpfiles.d/zpl-lock.conf /usr/local/bin/zpl-send
rm -rf /run/lock/zpl
if getent group zplraw >/dev/null 2>&1; then
  gpasswd -d pi zplraw >/dev/null 2>&1 || true
  groupdel zplraw 2>/dev/null || true
fi
/usr/local/bin/zpl-agent --config /etc/zpl-agent/config.toml --check-config
systemctl daemon-reload
udevadm control --reload-rules
udevadm trigger --subsystem-match=usbmisc
udevadm settle
systemctl enable zpl-agent
systemctl restart zpl-agent
if [ "$PROFILE" = zero2w ] && [ -e /etc/systemd/system/audio-buttons.service ]; then
  systemctl try-restart audio-buttons.service || true
fi
echo "zpl-agent $VERSION installed for $target${PROFILE:+ ($PROFILE)}"
