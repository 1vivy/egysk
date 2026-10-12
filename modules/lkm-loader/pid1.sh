#!/egysk/bin/busybox sh
set -eu
MODDIR=${0%/*}
export MODDIR
"$MODDIR/bin/lkm-loader" --vendor
"$MODDIR/bin/lkm-loader" --dir "$MODDIR/kmod"
