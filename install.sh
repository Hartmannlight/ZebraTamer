#!/bin/sh
set -eu

REPOSITORY="${ZPL_AGENT_REPOSITORY:-Hartmannlight/ZebraTamer}"
VERSION="${ZPL_AGENT_VERSION:-latest}"
BASE="https://github.com/${REPOSITORY}/releases"

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

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
asset="zpl-agent-$target"
curl -fsSL "$url/$asset" -o "$tmp/$asset"
curl -fsSL "$url/$asset.sha256" -o "$tmp/zpl-agent.sha256"
(cd "$tmp" && sha256sum -c zpl-agent.sha256)

install -m 0755 "$tmp/$asset" /usr/local/bin/zpl-agent
getent group zpl-agent >/dev/null 2>&1 || groupadd --system zpl-agent
id zpl-agent >/dev/null 2>&1 || useradd --system --gid zpl-agent --groups lp --home-dir /var/lib/zpl-agent --shell /usr/sbin/nologin zpl-agent
install -d -o zpl-agent -g zpl-agent -m 0750 /var/lib/zpl-agent
install -d -o root -g zpl-agent -m 0750 /etc/zpl-agent

if [ ! -e /etc/zpl-agent/config.toml ]; then
  curl -fsSL "$url/config.example.toml" -o /etc/zpl-agent/config.toml
  chown root:zpl-agent /etc/zpl-agent/config.toml
  chmod 0640 /etc/zpl-agent/config.toml
fi
curl -fsSL "$url/zpl-agent.service" -o /etc/systemd/system/zpl-agent.service
curl -fsSL "$url/70-zpl-agent-device.rules" -o /etc/udev/rules.d/70-zpl-agent-device.rules
curl -fsSL "$url/zpl-agent.modules.conf" -o /etc/modules-load.d/zpl-agent.conf
/usr/local/bin/zpl-agent --config /etc/zpl-agent/config.toml --check-config
systemctl daemon-reload
udevadm control --reload-rules
systemctl enable --now zpl-agent
echo "zpl-agent $VERSION installed for $target"
