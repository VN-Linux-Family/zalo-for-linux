#!/bin/bash
rm -f /usr/share/pixmaps/zalo.png
if [ "$1" != "upgrade" ]; then
  rm -rf /opt/Zalo/app/native/wine-runtime
  # Camera bridge cleanup
  rm -f /etc/modules-load.d/zalo-camera.conf
  rm -f /etc/modprobe.d/zalo-camera.conf
  # Remove old systemd service if present (from earlier versions)
  systemctl disable zcall-camera-sysfs.service 2>/dev/null || true
  rm -f /etc/systemd/system/zcall-camera-sysfs.service
  # Clean old config names too
  rm -f /etc/modules-load.d/v4l2loopback.conf
  rm -f /etc/modprobe.d/v4l2loopback.conf
  rm -rf /tmp/fake_video*
fi
exit 0
