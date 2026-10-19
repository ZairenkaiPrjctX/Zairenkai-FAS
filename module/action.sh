#!/system/bin/sh
# Manager "Action" button: show the current status.
MODDIR=${0%/*}
"$MODDIR/bin/zfasd" ctl --dir /data/adb/zairenkai-fas status 2>&1 | head -c 4000
echo
