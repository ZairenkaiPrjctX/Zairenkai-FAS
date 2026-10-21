#!/system/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 ZairenkaiPrjctX contributors
# Manager "Action" button: show the current status.
MODDIR=${0%/*}
"$MODDIR/bin/zfasd" ctl --dir /data/adb/zairenkai-fas status 2>&1 | head -c 4000
echo
