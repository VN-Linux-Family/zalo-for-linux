#!/usr/bin/env bash
# During a variant replacement, the newly installed package may already own
# /opt/Zalo/zalo. Do not let the old package's post-remove hook delete its link.
if [[ -x /opt/Zalo/zalo ]]; then exit 0; fi
if command -v update-alternatives >/dev/null 2>&1; then
  update-alternatives --remove zalo /opt/Zalo/zalo || true
elif [[ -L /usr/bin/zalo && "$(readlink /usr/bin/zalo)" == /opt/Zalo/zalo ]]; then
  rm -f /usr/bin/zalo
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database /usr/share/applications || true
fi
