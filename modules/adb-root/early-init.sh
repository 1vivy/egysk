#!/system/bin/sh
set -eu
MODDIR=${0%/*}
/dev/egysk/bin/resetprop egysk.adb_root.ready 1
exec /dev/egysk/bin/busybox sh "$MODDIR/setup.sh"
