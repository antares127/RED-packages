use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default = "default_format")]
    pub format: u32,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default = "default_release")]
    pub release: u32,
    #[serde(default = "default_arch")]
    pub arch: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub license: Vec<String>,
    #[serde(default)]
    pub maintainer: String,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub opt_depends: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub replaces: Vec<String>,
    /// Dependency strings copied from a foreign package format. Informational only.
    #[serde(default)]
    pub foreign_depends: Vec<String>,
    #[serde(default)]
    pub foreign_conflicts: Vec<String>,
    #[serde(default)]
    pub payload: String,
    #[serde(default)]
    pub compression: String,
    #[serde(default)]
    pub payload_size: u64,
    #[serde(default)]
    pub installed_size: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub files: Vec<String>,
    /// phase -> archive member name (hooks/<phase>.sh)
    #[serde(default)]
    pub hooks: BTreeMap<String, String>,
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub built_at: u64,
    #[serde(default = "default_red_version")]
    pub red_version: String,
}

fn default_format() -> u32 {
    FORMAT_VERSION
}
fn default_release() -> u32 {
    1
}
fn default_arch() -> String {
    "any".to_string()
}
fn default_red_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            format: FORMAT_VERSION,
            name: String::new(),
            version: String::new(),
            release: 1,
            arch: default_arch(),
            description: String::new(),
            url: String::new(),
            license: Vec::new(),
            maintainer: String::new(),
            depends: Vec::new(),
            opt_depends: Vec::new(),
            provides: Vec::new(),
            conflicts: Vec::new(),
            replaces: Vec::new(),
            foreign_depends: Vec::new(),
            foreign_conflicts: Vec::new(),
            payload: String::new(),
            compression: "none".to_string(),
            payload_size: 0,
            installed_size: 0,
            sha256: String::new(),
            files: Vec::new(),
            hooks: BTreeMap::new(),
            origin: String::new(),
            built_at: 0,
            red_version: default_red_version(),
        }
    }
}

impl Manifest {
    pub fn full_version(&self) -> String {
        if self.release <= 1 {
            self.version.clone()
        } else {
            format!("{}-{}", self.version, self.release)
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.format != FORMAT_VERSION {
            bail!(
                "unsupported RED format version {} (expected {})",
                self.format,
                FORMAT_VERSION
            );
        }
        crate::util::validate_package_name(&self.name)?;
        if self.version.trim().is_empty() {
            bail!("manifest is missing a version");
        }
        if self.payload.is_empty() {
            bail!("manifest is missing a payload");
        }
        Ok(())
    }

    pub fn pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Optional declarative build recipe used by `red -build`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Redfile {
    pub name: Option<String>,
    pub version: Option<String>,
    pub release: Option<u32>,
    pub arch: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub license: Vec<String>,
    pub maintainer: Option<String>,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub opt_depends: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub replaces: Vec<String>,
    /// phase -> path relative to the Redfile
    #[serde(default)]
    pub hooks: BTreeMap<String, String>,
    /// Optional subdirectory used as payload root.
    pub payload_dir: Option<String>,
    /// Paths (relative to the payload root) to exclude.
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Redfile {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read Redfile {}", path.display()))?;
        let redfile: Redfile = serde_json::from_str(&text)
            .with_context(|| format!("invalid Redfile {}", path.display()))?;
        Ok(redfile)
    }
}

pub fn normalize_hook_phase(phase: &str) -> String {
    phase.replace('-', "_")
}

pub fn valid_hook_phases() -> &'static [&'static str] {
    &[
        "pre_install",
        "post_install",
        "pre_remove",
        "post_remove",
        "arch_install",
    ]
}
