//! Upstream Magiskinit policy-loader permissions with product type names.
use magiskpolicy::ffi::SePolicy;

pub fn bootstrap(sepol: &mut SePolicy) {
    sepol.typeattribute(vec!["egysk"], vec!["mlstrustedsubject"]);
    sepol.typeattribute(vec!["egysk_file", "egysk_log_file"], vec!["mlstrustedobject"]);
    sepol.allow(vec!["kernel"], vec!["rootfs", "tmpfs"], vec!["chr_file"], vec!["write"]);
    sepol.allow(vec!["kernel"], vec!["tmpfs"], vec!["fifo_file"], vec!["open", "read", "write"]);
    sepol.allow(vec!["rootfs"], vec!["labeledfs", "tmpfs"], vec!["filesystem"], vec!["associate"]);
    sepol.allow(vec!["kernel"], vec![], vec!["file"], vec!["relabelto"]);
    sepol.allow(vec!["kernel"], vec!["tmpfs"], vec!["file"], vec!["relabelfrom"]);
    sepol.allow(vec!["kernel"], vec!["tmpfs"], vec!["dir", "lnk_file", "fifo_file"], vec!["relabelfrom"]);
    sepol.allow(vec!["kernel"], vec!["egysk_file"], vec!["dir", "lnk_file", "fifo_file"], vec!["relabelto"]);
    sepol.allow(vec!["kernel"], vec!["egysk_file"], vec!["dir"], vec!["search", "open", "read", "getattr"]);
    sepol.allow(vec!["kernel"], vec!["egysk_file"], vec!["file", "lnk_file", "fifo_file"], vec!["getattr"]);
    sepol.allow(vec!["kernel"], vec!["kernel"], vec!["process"], vec!["setcurrent"]);
    sepol.allow(vec!["kernel"], vec!["egysk"], vec!["process"], vec!["dyntransition"]);
    sepol.allow(vec!["init", "egysk"], vec!["proc_sysrq"], vec!["file"], vec!["open", "write"]);
    sepol.allow(vec!["egysk"], vec!["egysk_file"], vec!["blk_file"],
        vec!["create", "open", "read", "write", "getattr", "setattr", "ioctl", "unlink"]);
    sepol.allow(vec!["egysk"], vec!["init"], vec!["process"], vec!["signal"]);
}

pub fn label_files() {
    use base::{cstr, pre_order_walk, WalkResult};
    let root = cstr!("/dev/egysk");
    let result = (|| -> base::LoggedResult<()> {
        root.set_secontext(cstr!("u:object_r:egysk_file:s0"))?;
        pre_order_walk(root, |_, path| {
            path.set_secontext(cstr!("u:object_r:egysk_file:s0"))?;
            Ok(WalkResult::Continue)
        })?;
        cstr!("/dev/esp").set_secontext(cstr!("u:object_r:egysk_file:s0"))?;
        Ok(())
    })();
    if result.is_err() {
        egysk_runtime::fatal_boot("native runtime labeling failed");
    }
}
