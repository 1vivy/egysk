use anyhow::{Context, Result, bail};
use std::{ffi::CString, path::Path};

fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let module = args
        .next()
        .context("usage: lkm-loader --vendor | MODULE [PARAMS]")?;
    if module == "--vendor" {
        if args.next().is_some() {
            bail!("--vendor takes no arguments");
        }
        return lkm_loader::loader::load_vendor_modules();
    }
    let params = args.next().unwrap_or_default();
    if args.next().is_some() {
        bail!("usage: lkm-loader MODULE [PARAMS]");
    }
    let params = params.to_str().context("module parameters are not UTF-8")?;
    let params = lkm_loader::loader::substitute_by_name_params(params)?;
    let params = CString::new(params).context("module parameters contain NUL")?;
    let path = Path::new(&module);
    let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    lkm_loader::load_module(data, &params).with_context(|| format!("load {}", path.display()))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("lkm-loader: {error:#}");
        std::process::exit(1);
    }
}
