#!/usr/bin/env bash
set -Eeuo pipefail

getent group zpl-agent >/dev/null || groupadd --system zpl-agent
id zpl-agent >/dev/null 2>&1 || useradd --system --gid zpl-agent \
  --home-dir /var/lib/zpl-agent --shell /usr/sbin/nologin zpl-agent

install -d -o zpl-agent -g zpl-agent -m 0750 /var/lib/zpl-agent
install -d -o root -g zpl-agent -m 0750 /etc/zpl-agent
install -m 0755 /home/pi/zpl-agent-src/target/release/zpl-agent /usr/local/bin/zpl-agent
install -o root -g zpl-agent -m 0640 /home/pi/config.toml /etc/zpl-agent/config.toml
install -o root -g root -m 0644 /home/pi/zpl-agent.service /etc/systemd/system/zpl-agent.service
install -o root -g root -m 0644 /home/pi/70-zpl-agent-device.rules /etc/udev/rules.d/70-zpl-agent-device.rules
install -o root -g root -m 0644 /home/pi/zpl-agent.modules.conf /etc/modules-load.d/zpl-agent.conf

# The audio-button service now submits to the agent API. It must no longer
# receive a direct printer alias or a separate raw sender.
rm -f /etc/udev/rules.d/70-zpl-usb-parallel-ente.rules
rm -f /etc/tmpfiles.d/zpl-lock.conf
rm -f /etc/modules-load.d/zpl-usblp.conf
rm -f /usr/local/bin/zpl-send
rm -rf /run/lock/zpl
if getent group zplraw >/dev/null; then
  gpasswd -d pi zplraw >/dev/null 2>&1 || true
  groupdel zplraw
fi

udevadm control --reload-rules
udevadm trigger --subsystem-match=usbmisc
udevadm settle
systemctl daemon-reload
systemctl enable zpl-agent.service
systemctl restart zpl-agent.service
