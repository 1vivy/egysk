#!/system/bin/sh
# Called only by debug-boot RC. No persistent properties or /data dependency.
set -eu
export ASH_STANDALONE=1
PATH=/dev/egysk/bin:/system/bin
export PATH
bin=/dev/egysk/bin
apex=/apex/com.android.adbd
prop="$bin/resetprop"
"$prop" -n ro.adb.secure 0
"$prop" -n service.adb.root 1

# init may still be in the bootstrap mount namespace. The ready/post-fs-data
# callbacks also run in the default namespace after APEX activation.
if [ ! -x "$apex/bin/adbd" ]; then
    echo 'egysk: debug adb waiting for adbd APEX' > /dev/kmsg
    exit 0
fi

installed=0
if [ ! -f "$apex/bin/libegysk-adb.so" ]; then
    # Preserve the real executable under the same APEX path: Android linker
    # namespace selection depends on that path, not the original inode.
    key=$(stat -c '%d-%i' "$apex/bin/adbd")
    view="/dev/egysk/adb/$key"
    if [ ! -d "$view" ]; then
        mkdir -p "$view"
        "$bin/egyskd" --clone "$apex/bin" "$view/bin"
        mv "$view/bin/adbd" "$view/bin/adbd.real"
        cp "$bin/egysk-adbd" "$view/bin/adbd"
        cp "$bin/libegysk-adb.so" "$view/bin/libegysk-adb.so"
        "$bin/egyskd" --clone-attr "$view/bin/adbd.real" "$view/bin/adbd"
        "$bin/egyskd" --clone-attr "$view/bin/adbd.real" "$view/bin/libegysk-adb.so"
    fi
    mount --bind "$view/bin" "$apex/bin"
    installed=1
fi

# A missing property is normal before stock USB init; resetprop returns nonzero.
config=$("$prop" sys.usb.config || :)
case ",$config," in
    *,adb,*) ;;
    ,none,|,,) "$prop" sys.usb.config adb ;;
    *) "$prop" sys.usb.config "$config,adb" ;;
esac
if [ "$installed" = 1 ]; then
    "$prop" ctl.restart adbd
    echo 'egysk: debug root adbd enabled (authentication disabled)' > /dev/kmsg
fi
