#!/system/bin/sh
# Late-start service: supervise zfasd with exponential backoff.
MODDIR=${0%/*}
DATA=/data/adb/zairenkai-fas
BIN="$MODDIR/bin/zfasd"
LOG="$DATA/zfasd.log"
mkdir -p "$DATA" && chmod 0700 "$DATA"

until [ "$(getprop sys.boot_completed)" = "1" ]; do sleep 5; done
sleep 10

rotate() {
  if [ -f "$LOG" ] && [ "$(stat -c %s "$LOG" 2>/dev/null || echo 0)" -gt 524288 ]; then
    mv -f "$LOG" "$LOG.1"
  fi
}

delay=5
while [ ! -f "$MODDIR/disable" ] && [ ! -f "$MODDIR/remove" ]; do
  rotate
  started=$(date +%s)
  "$BIN" run --dir "$DATA" >>"$LOG" 2>&1
  code=$?
  ran=$(( $(date +%s) - started ))
  echo "service.sh: zfasd exited with $code after ${ran}s" >>"$LOG"
  # Whatever happened, make sure nothing is left boosted.
  "$BIN" restore --dir "$DATA" >>"$LOG" 2>&1
  [ "$code" -eq 0 ] && break
  if [ "$ran" -gt 600 ]; then delay=5; fi
  sleep "$delay"
  delay=$(( delay * 2 )); [ "$delay" -gt 300 ] && delay=300
done
