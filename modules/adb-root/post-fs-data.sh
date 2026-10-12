#!/system/bin/sh
set -eu
MODDIR=${0%/*}
exec /dev/egysk/bin/busybox sh "$MODDIR/setup.sh"
