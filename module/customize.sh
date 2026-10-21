#!/system/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 ZairenkaiPrjctX contributors
# Install-time checks for Magisk/KernelSU. Sourced by the installer ($MODPATH, ui_print,
# abort and set_perm are provided by it).

[ "$ARCH" = "arm64" ] || abort "! Zairenkai-FAS supports arm64 only (found $ARCH)"
[ "$API" -ge 29 ] || abort "! Android 10 (API 29) or newer is required (found API $API)"

ui_print "- Zairenkai-FAS: installing to $MODPATH"
DATA=/data/adb/zairenkai-fas
mkdir -p "$DATA"
chmod 0700 "$DATA"
if [ -f "$DATA/config.toml" ]; then
  ui_print "- Keeping existing $DATA/config.toml"
fi

set_perm_recursive "$MODPATH" 0 0 0755 0644
set_perm "$MODPATH/bin/zfasd" 0 0 0755
set_perm "$MODPATH/service.sh" 0 0 0755
set_perm "$MODPATH/uninstall.sh" 0 0 0755
set_perm "$MODPATH/action.sh" 0 0 0755

ui_print "- Capability probe (nothing is changed):"
"$MODPATH/bin/zfasd" probe 2>/dev/null | grep -E '"(name|status)"' | paste - - | sed 's/[",]//g; s/  */ /g' | while read -r line; do
  ui_print "    $line"
done
ui_print "- Configuration: $DATA/config.toml (defaults apply when absent)"
ui_print "- Status: $MODPATH/bin/zfasd ctl status"
