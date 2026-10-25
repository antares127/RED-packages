use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Convert an archive entry path to a safe path relative to the install root.
/// Returns `None` for paths that resolve to the root itself (e.g. "./").
pub fn sanitize_relative(path: &Path) -> Result<Option<PathBuf>> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => out.push(name),
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                bail!("unsafe path in archive: {}", path.display());
            }
        }
    }
    if out.as_os_str().is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

pub fn rel_to_string(path: &Path) -> Result<String> {
    let s = path.to_string_lossy();
    if s.is_empty() {
        bail!("empty path");
    }
    Ok(s.into_owned())
}

pub fn is_arch_metadata(rel: &str) -> bool {
    matches!(
        rel,
        ".PKGINFO" | ".INSTALL" | ".BUILDINFO" | ".MTREE" | ".red" | ".red-meta.json"
    )
}

pub fn validate_package_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("package name is empty");
    }
    if name.len() > 128 {
        bail!("package name is too long");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '+' | '-'))
    {
        bail!("invalid package name: {name}");
    }
    if name.contains("..") || name.starts_with('.') || name.starts_with('-') {
        bail!("invalid package name: {name}");
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn write_executable(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::write(path, data).with_context(|| format!("cannot write {}", path.display()))?;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

pub fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_dir() {
                bail!("refusing to remove directory {}", path.display());
            }
            fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))?;
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("cannot stat {}", path.display())),
    }
    Ok(())
}

/// Remove empty parent directories, stopping at `stop` (exclusive).
pub fn prune_empty_parents(path: &Path, stop: &Path) {
    let mut current = path.parent();
    while let Some(dir) = current {
        if dir == stop || !dir.starts_with(stop) {
            break;
        }
        match fs::remove_dir(dir) {
            Ok(()) => current = dir.parent(),
            Err(_) => break,
        }
    }
}

pub fn print_field(label: &str, value: &str) {
    if !value.is_empty() {
        println!("{label:<18}: {value}");
    }
}
