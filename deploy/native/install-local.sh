#!/usr/bin/env bash
set -Eeuo pipefail

if [ "$(id -u)" -ne 0 ]; then
  echo "Run this installer as root." >&2
  exit 1
fi

package_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
binary="${1:-$package_dir/zpl-agent}"
config="${2:-$package_dir/config.toml}"
token_source="${ZEBRATAMER_TOKEN_FILE:-}"

for required in "$binary" "$config" "$package_dir/zpl-agent.service" "$package_dir/70-zpl-agent-device.rules"; do
  if [ ! -f "$required" ]; then
    echo "Missing installation file: $required" >&2
    exit 1
  fi
done

if ! getent group lp >/dev/null; then
  echo "The required system group 'lp' does not exist." >&2
  exit 1
fi
getent group zpl-agent >/dev/null || groupadd --system zpl-agent
id zpl-agent >/dev/null 2>&1 || useradd --system --gid zpl-agent \
  --home-dir /var/lib/zpl-agent --shell /usr/sbin/nologin zpl-agent
usermod --append --groups lp zpl-agent

install -d -o zpl-agent -g zpl-agent -m 0750 /var/lib/zpl-agent
install -d -o root -g zpl-agent -m 0750 /etc/zpl-agent

if [ -n "$token_source" ]; then
  if [ ! -s "$token_source" ]; then
    echo "Configured token file is missing or empty: $token_source" >&2
    exit 1
  fi
  install -o root -g zpl-agent -m 0640 "$token_source" /etc/zpl-agent/token
elif [ ! -s /etc/zpl-agent/token ]; then
  umask 0077
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 32 > /etc/zpl-agent/token
  else
    head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > /etc/zpl-agent/token
  fi
fi
chown root:zpl-agent /etc/zpl-agent/token
chmod 0640 /etc/zpl-agent/token

install -o root -g zpl-agent -m 0640 "$config" /etc/zpl-agent/config.toml.new
install -o root -g root -m 0755 "$binary" /usr/local/bin/zpl-agent.new
/usr/local/bin/zpl-agent.new --config /etc/zpl-agent/config.toml.new --check-config

if [ -x /usr/local/bin/zpl-agent ]; then
  cp --preserve=mode,ownership,timestamps /usr/local/bin/zpl-agent /usr/local/bin/zpl-agent.previous
fi
mv /usr/local/bin/zpl-agent.new /usr/local/bin/zpl-agent
mv /etc/zpl-agent/config.toml.new /etc/zpl-agent/config.toml

install -o root -g root -m 0644 "$package_dir/zpl-agent.service" /etc/systemd/system/zpl-agent.service
install -o root -g root -m 0644 "$package_dir/70-zpl-agent-device.rules" /etc/udev/rules.d/70-zpl-agent-device.rules
udevadm control --reload-rules
udevadm trigger --subsystem-match=usbmisc
udevadm settle
systemctl daemon-reload
systemctl enable zpl-agent.service
systemctl restart zpl-agent.service

for _attempt in 1 2 3 4 5 6 7 8 9 10; do
  if /usr/local/bin/zpl-agent --healthcheck http://127.0.0.1:8080/healthz; then
    echo "ZebraTamer installation is healthy."
    exit 0
  fi
  sleep 1
done

systemctl --no-pager --full status zpl-agent.service >&2 || true
exit 1
