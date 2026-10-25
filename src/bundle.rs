//! `.ared` bundle archives: a collection of `.red` packages packed from a
//! directory, keeping its layout. Installing a bundle installs every package
//! it contains in one go.

use crate::archive;
use crate::build::sanitize_name;
use crate::util::{human_size, now_unix, validate_package_name};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tar::{Archive, Builder, Header};
use walkdir::WalkDir;

pub const BUNDLE_FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleEntry {
    /// Path of the .red file inside the bundle, relative to the source dir.
    pub path: String,
    pub name: String,
    pub version: String,
    pub arch: String,
    #[serde(default)]
    pub description: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    pub format: u32,
    pub kind: String,
    pub name: String,
    pub built_at: u64,
    pub packages: Vec<BundleEntry>,
}

impl BundleManifest {
    pub fn total_size(&self) -> u64 {
        self.packages.iter().map(|entry| entry.size).sum()
    }
}

/// Collect every `.red` file under `dir`, preserving relative paths.
pub fn find_red_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(dir).follow_links(false).sort_by_file_name() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if name.to_ascii_lowercase().ends_with(".red") {
            files.push(entry.path().to_path_buf());
        }
    }
    Ok(files)
}

/// Pack all `.red` files under `dir` into a single `.ared` bundle.
pub fn build_bundle(dir: &Path, name: Option<String>, out: Option<PathBuf>) -> Result<PathBuf> {
    if !dir.is_dir() {
        bail!("bundle source must be a directory: {}", dir.display());
    }
    println!("==> Building ARED bundle from {}", dir.display());

    let name = match name {
        Some(name) => name,
        None => {
            let raw = dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "bundle".to_string());
            let name = sanitize_name(&raw);
            if name.is_empty() {
                "bundle".to_string()
            } else {
                name
            }
        }
    };
    validate_package_name(&name)?;

    let files = find_red_files(dir)?;
    if files.is_empty() {
        bail!("no .red packages found under {}", dir.display());
    }

    let mut packages = Vec::new();
    let mut members: Vec<(String, PathBuf)> = Vec::new();
    for file in &files {
        let rel = file.strip_prefix(dir)?.to_string_lossy().replace('\\', "/");
        let manifest = match archive::read_manifest(file) {
            Ok(manifest) => manifest,
            Err(err) => {
                eprintln!("red: warning: skipping {}: {err:#}", file.display());
                continue;
            }
        };
        let version = manifest.full_version();
        packages.push(BundleEntry {
            path: rel.clone(),
            name: manifest.name,
            version,
            arch: manifest.arch,
            description: manifest.description,
            size: fs::metadata(file).map(|meta| meta.len()).unwrap_or(0),
        });
        members.push((rel, file.clone()));
    }
    if packages.is_empty() {
        bail!("no valid .red packages found under {}", dir.display());
    }

    let manifest = BundleManifest {
        format: BUNDLE_FORMAT,
        kind: "ared".to_string(),
        name: name.clone(),
        built_at: now_unix(),
        packages,
    };

    let out = match out {
        Some(out) => out,
        None => dir
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!("{name}.ared")),
    };
    let parent = out.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }

    let tmp = tempfile::Builder::new()
        .prefix(".ared-build-")
        .tempfile_in(parent.unwrap_or(Path::new(".")))?;
    {
        let mut builder = Builder::new(tmp.as_file());
        let data = serde_json::to_string_pretty(&manifest)?;
        append_bytes(&mut builder, "manifest.json", data.as_bytes())?;
        for (rel, file) in &members {
            let mut input = File::open(file)
                .with_context(|| format!("cannot open {}", file.display()))?;
            let size = input.metadata()?.len();
            let mut header = Header::new_gnu();
            header.set_path(rel)?;
            header.set_size(size);
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_cksum();
            builder
                .append_data(&mut header, rel, &mut input)
                .with_context(|| format!("cannot append {rel} to bundle"))?;
        }
        builder.finish().context("cannot finish .ared archive")?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist(&out)
        .map_err(|err| anyhow::anyhow!("cannot write {}: {}", out.display(), err.error))?;
    fs::set_permissions(&out, fs::Permissions::from_mode(0o644))?;

    println!(
        "==> Created {} ({} packages, {})",
        out.display(),
        manifest.packages.len(),
        human_size(manifest.total_size())
    );
    Ok(out)
}

fn append_bytes(builder: &mut Builder<&File>, name: &str, data: &[u8]) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_path(name)?;
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    builder.append(&header, data)?;
    Ok(())
}

/// Read only the bundle manifest, without extracting the packages.
pub fn read_bundle(path: &Path) -> Result<BundleManifest> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut archive = Archive::new(BufReader::new(file));
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_string_lossy() == "manifest.json" {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            let manifest: BundleManifest =
                serde_json::from_str(&text).context("invalid manifest.json inside .ared")?;
            if manifest.format != BUNDLE_FORMAT || manifest.kind != "ared" {
                bail!("{} is not a supported .ared bundle", path.display());
            }
            return Ok(manifest);
        }
    }
    bail!("{} is not a valid .ared bundle (no manifest.json)", path.display());
}

/// Extract every `.red` member of the bundle into `dst`, keeping the stored
/// directory layout. Returns the extracted paths in a stable order.
pub fn extract_bundle(path: &Path, dst: &Path) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(dst)?;
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut archive = Archive::new(BufReader::new(file));
    let mut members = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw = entry.path()?.into_owned();
        let name = raw.to_string_lossy().to_string();
        if name == "manifest.json" || !name.to_ascii_lowercase().ends_with(".red") {
            continue;
        }
        let Some(rel) = crate::util::sanitize_relative(&raw)? else {
            continue;
        };
        let target = dst.join(&rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        entry
            .unpack(&target)
            .with_context(|| format!("cannot extract {name}"))?;
        members.push(target);
    }
    members.sort();
    Ok(members)
}
