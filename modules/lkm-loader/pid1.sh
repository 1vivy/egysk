#!/egysk/bin/busybox sh
set -eu
MODDIR=${0%/*}
export MODDIR
"$MODDIR/bin/lkm-loader" --vendor
if [ -e "$MODDIR/kmod/modules.load" ]; then
    "$MODDIR/bin/lkm-loader" --dir "$MODDIR/kmod"
fi
