use anyhow::{Context, Result, bail};
use std::{ffi::CString, path::Path};

/// Assisted insmod: resolve by-name parameters, then load one module file.
fn assisted_insmod(path: &Path, params: &str) -> Result<()> {
    let params = lkm_loader::loader::substitute_by_name_params(params)?;
    let params = CString::new(params).context("module parameters contain NUL")?;
    let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    lkm_loader::load_module(data, &params)
}

/// Load one module directory's payload list in file order, stopping at the
/// first failure. The whole list is parsed before any module is loaded.
fn load_directory(dir: &Path) -> Result<()> {
    for (file, params) in lkm_loader::loader::payload_module_list(dir)? {
        let path = dir.join(&file);
        assisted_insmod(&path, &params).with_context(|| file.clone())?;
    }
    Ok(())
}

fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let module = args
        .next()
        .context("usage: lkm-loader --vendor | --dir DIR | MODULE [PARAMS]")?;
    if module == "--vendor" {
        if args.next().is_some() {
            bail!("--vendor takes no arguments");
        }
        return lkm_loader::loader::load_vendor_modules();
    }
    if module == "--dir" {
        let dir = args.next().context("--dir needs the module directory")?;
        if args.next().is_some() {
            bail!("--dir takes exactly one directory");
        }
        return load_directory(Path::new(&dir));
    }
    let params = args.next().unwrap_or_default();
    if args.next().is_some() {
        bail!("usage: lkm-loader MODULE [PARAMS]");
    }
    let params = params.to_str().context("module parameters are not UTF-8")?;
    let path = Path::new(&module);
    assisted_insmod(path, params).with_context(|| format!("load {}", path.display()))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("lkm-loader: {error:#}");
        std::process::exit(1);
    }
}
