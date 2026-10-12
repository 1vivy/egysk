#!/system/bin/sh
# Ordinary module setup: no core RC, policy injection or USB-config trigger.
set -eu
export ASH_STANDALONE=1
PATH=/dev/egysk/bin:/system/bin
export PATH
MODDIR=${0%/*}
bin=/dev/egysk/bin
prop="$bin/resetprop"
apex=/apex/com.android.adbd
payload="$MODDIR/system/apex/com.android.adbd/bin"

# Native module projection also loads system.prop. Load it here for the early
# hook, before post-fs projection, so any adbd we start has the same properties.
"$prop" -n --file "$MODDIR/system.prop"

# Do not race load_persist_props or write its later-overwritten in-memory value.
# The module's persistent-ready RC action retries this after the disk load.
if [ "$("$prop" ro.persistent_properties.ready || :)" = true ]; then
    config=$("$prop" persist.sys.usb.config || :)
    case ",$config," in
        *,adb,*) ;;
        ,none,|,,) "$prop" persist.sys.usb.config adb ;;
        *) "$prop" persist.sys.usb.config "$config,adb" ;;
    esac
fi

if [ ! -x "$apex/bin/adbd" ]; then
    echo 'adb-root: waiting for adbd APEX; ready/post-fs-data will retry' > /dev/kmsg
    exit 0
fi
if [ "$apex/bin/adbd" -ef "$payload/adbd" ]; then
    exit 0
fi

# Like tiann/adb_root, retain stock adbd beside the wrapper and bind the module
# bin directory over the APEX bin directory. Capture this boot's real binary,
# not one from the producer's phone or a previous firmware version.
cp -f "$apex/bin/adbd" "$payload/adbd.real.new"
"$bin/egyskd" --clone-attr "$apex/bin/adbd" "$payload/adbd.real.new"
mv -f "$payload/adbd.real.new" "$payload/adbd.real"
"$bin/egyskd" --clone-attr "$apex/bin/adbd" "$payload/adbd"
"$bin/egyskd" --clone-attr "$apex/bin/adbd" "$payload/libadb-root.so"
mount --bind "$payload" "$apex/bin"
"$prop" ctl.restart adbd
echo 'adb-root: stock adbd wrapped; root and no-auth enabled' > /dev/kmsg
