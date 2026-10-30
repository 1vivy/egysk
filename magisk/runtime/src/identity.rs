//! Early boot identity from the configured provider. Module admission runs
//! before any later property or identity phase, so the provider answer is read
//! once here and carried in the boot snapshot.
use crate::context::*;
use crate::fsutil as fsu;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The boot-control owner of this boot. `Unmanaged` is a direct boot that
/// records itself as `direct`, and `Missing` has no dispatch record at all.
/// Neither one is a virtual boot, so neither one admits a `virtual_only` module.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BootOwner {
    Virtual,
    Physical,
    Unmanaged,
    Missing,
}

impl BootOwner {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Virtual => "virtual",
            Self::Physical => "physical",
            Self::Unmanaged => "unmanaged",
            Self::Missing => "missing",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    owner: BootOwner,
}

fn parse(text: &str) -> Result<BootOwner> {
    Ok(serde_json::from_str::<Record>(text)?.owner)
}

/// Run the configured provider with `early-identity` before backing storage,
/// module discovery, package policy and RC admission.
///
/// Safe mode and charger boot discover no module, so they make no admission
/// decision and do not gate the boot on the provider. A configured provider
/// that fails, or an unknown record, stops the boot: there is no bootconfig
/// fallback and no other owner authority.
pub fn read(config: &BootstrapConfig, mode: BootMode) -> Result<Option<BootOwner>> {
    if config.safe_mode || mode == BootMode::Charger {
        return Ok(None);
    }
    let Some(provider) = &config.early_identity else {
        return Ok(None);
    };
    let output_path = Path::new(ROOT).join("early-identity.json");
    let output = fs::File::create(&output_path)?;
    let mut command = Command::new(Path::new(BIN).join(&provider.tool));
    crate::scripts::child_signals(&mut command);
    command
        .process_group(0)
        .arg("early-identity")
        .stdin(Stdio::null())
        .stdout(output);
    let mut child = command
        .spawn()
        .with_context(|| format!("start early identity provider {}", provider.tool))?;
    crate::scripts::wait_until(&mut child, Instant::now() + Duration::from_secs(35), false)
        .with_context(|| format!("early identity provider {} early-identity", provider.tool))?;
    parse(&fsu::text(&output_path, 256)?)
        .with_context(|| format!("invalid early identity record from {}", provider.tool))
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_named_owners_are_accepted() {
        assert_eq!(parse(r#"{"owner":"virtual"}"#).unwrap(), BootOwner::Virtual);
        assert_eq!(
            parse(r#"{"owner":"physical"}"#).unwrap(),
            BootOwner::Physical
        );
        assert_eq!(
            parse(r#"{"owner":"unmanaged"}"#).unwrap(),
            BootOwner::Unmanaged
        );
        assert_eq!(parse(r#"{"owner":"missing"}"#).unwrap(), BootOwner::Missing);
        assert_eq!(BootOwner::Virtual.as_str(), "virtual");
        assert_eq!(BootOwner::Missing.as_str(), "missing");
        assert!(parse(r#"{"owner":"Virtual"}"#).is_err());
        assert!(parse(r#"{"owner":"other"}"#).is_err());
        assert!(parse(r#"{"owner":"virtual","rom":"rom2"}"#).is_err());
        assert!(parse("").is_err());
    }
}
