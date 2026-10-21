#!/system/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 ZairenkaiPrjctX contributors
# Stop the daemon, restore original settings, remove module data.
MODDIR=${0%/*}
DATA=/data/adb/zairenkai-fas
for pid in $(pidof zfasd); do kill -TERM "$pid" 2>/dev/null; done
sleep 2
if "$MODDIR/bin/zfasd" restore --dir "$DATA"; then
  rm -rf "$DATA"
else
  # Keep restore.json so a later boot (or a manual restore) can retry; floors are not
  # persistent across reboots anyway.
  echo "zairenkai-fas: restore failed; data kept in $DATA" >>/cache/zairenkai-fas-uninstall.log 2>/dev/null
fi
