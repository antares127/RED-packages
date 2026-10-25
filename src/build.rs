use crate::archive::{self, Compression, ScanResult};
use crate::manifest::{valid_hook_phases, Manifest, Redfile};
use crate::util::{now_unix, sha256_file, validate_package_name};
use anyhow::{bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression as GzCompression;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tar::Archive;
use walkdir::WalkDir;

#[derive(Debug, Default, Clone)]
pub struct BuildOptions {
    pub name: Option<String>,
    pub version: Option<String>,
    pub release: Option<u32>,
    pub arch: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub license: Vec<String>,
    pub maintainer: Option<String>,
    pub depends: Vec<String>,
    pub provides: Vec<String>,
    pub conflicts: Vec<String>,
    pub replaces: Vec<String>,
    pub out: Option<PathBuf>,
    pub redfile: Option<PathBuf>,
    /// Build a native pacman package (.pkg.tar.gz) instead of a .red package.
    pub native: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Tar,
    Dir,
    Aur,
    Deb,
    Rpm,
    Arch,
}

fn detect_kind(source: &Path) -> Result<Kind> {
    if source.is_dir() {
        if source.join("PKGBUILD").exists() {
            return Ok(Kind::Aur);
        }
        return Ok(Kind::Dir);
    }
    if !source.exists() {
        bail!("no such file or directory: {}", source.display());
    }
    let lower = source.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".red") {
        bail!("{} is already a .red package", source.display());
    }
    if lower.ends_with(".deb") {
        return Ok(Kind::Deb);
    }
    if lower.ends_with(".rpm") {
        return Ok(Kind::Rpm);
    }
    if lower.ends_with(".pkg.tar")
        || lower.ends_with(".pkg.tar.gz")
        || lower.ends_with(".pkg.tar.xz")
        || lower.ends_with(".pkg.tar.zst")
    {
        return Ok(Kind::Arch);
    }
    if lower.ends_with(".tar")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".txz")
        || lower.ends_with(".tar.zst")
        || lower.ends_with(".tzst")
    {
        return Ok(Kind::Tar);
    }
    bail!(
        "unsupported source: {} (expected a directory, .tar.*, .deb, .rpm or .pkg.tar.*)",
        source.display()
    )
}

pub fn build(source: &Path, opts: &BuildOptions) -> Result<Vec<PathBuf>> {
    if opts.native {
        return Ok(vec![build_native(source, opts)?]);
    }
    match detect_kind(source)? {
        Kind::Tar => Ok(vec![build_tar(source, opts)?]),
        Kind::Dir => Ok(vec![build_dir(source, opts)?]),
        Kind::Deb => bail!(
            "{} is a DEB package; use 'red -rebuild {}' to convert it",
            source.display(),
            source.display()
        ),
        Kind::Rpm => bail!(
            "{} is an RPM package; use 'red -rebuild {}' to convert it",
            source.display(),
            source.display()
        ),
        Kind::Arch => bail!(
            "{} is an Arch package; use 'red -rebuild {}' to convert it",
            source.display(),
            source.display()
        ),
        Kind::Aur => bail!(
            "{} contains a PKGBUILD; use 'red -rebuild {}' to build it with makepkg",
            source.display(),
            source.display()
        ),
    }
}

pub fn rebuild(source: &Path, opts: &BuildOptions) -> Result<Vec<PathBuf>> {
    match detect_kind(source)? {
        Kind::Tar => Ok(vec![build_tar(source, opts)?]),
        Kind::Dir => Ok(vec![build_dir(source, opts)?]),
        Kind::Deb => Ok(vec![build_deb(source, opts)?]),
        Kind::Rpm => Ok(vec![build_rpm(source, opts)?]),
        Kind::Arch => Ok(vec![build_arch(source, opts, "arch")?]),
        Kind::Aur => build_aur(source, opts),
    }
}

fn build_tar(source: &Path, opts: &BuildOptions) -> Result<PathBuf> {
    println!("==> Building RED package from {}", source.display());
    let redfile = load_redfile(source, opts)?;
    let tmp = tempfile::tempdir()?;
    let compression = archive::tar_compression(source)?;
    let tar_path = archive::decompress_to_temp(source, compression, tmp.path())?;
    let scan = archive::scan_tar(tar_path.path(), false)?;
    let mut manifest = make_manifest(source, opts, redfile.as_ref(), "tar")?;
    let payload_sha = sha256_file(source)?;
    let hooks = attach_hooks(&mut manifest, source, redfile.as_ref())?;

    // Self-contained app tarballs (loose files with a launcher script but no
    // desktop entry) get a proper FHS layout so the installed package shows up
    // in the launcher menu.
    if redfile.is_none() {
        if let Some(plan) = detect_desktop_plan(&scan.files, &manifest.name) {
            let staging = tmp.path().join("staging");
            extract_tar(tar_path.path(), &staging)?;
            apply_desktop_layout(&staging, &manifest, &plan)?;
            return pack_staged_payload(source, opts, tmp.path(), staging, manifest, hooks, "tar");
        }
    }

    finish_manifest(&mut manifest, compression, &scan, payload_sha, source)?;
    let out = output_path(source, &manifest, opts)?;
    archive::write_red(&out, &manifest, source, &hooks)?;
    report(&out, &manifest);
    Ok(out)
}

fn build_dir(source: &Path, opts: &BuildOptions) -> Result<PathBuf> {
    println!("==> Building RED package from directory {}", source.display());
    let redfile = load_redfile(source, opts)?;
    let mut manifest = make_manifest(source, opts, redfile.as_ref(), "dir")?;
    let hooks = attach_hooks(&mut manifest, source, redfile.as_ref())?;
    let tmp = tempfile::tempdir()?;
    let payload = tmp.path().join("payload.tar.gz");
    create_payload_from_dir(source, redfile.as_ref(), &payload, true)?;
    let compression = Compression::Gzip;
    let tar_path = archive::decompress_to_temp(&payload, compression, tmp.path())?;
    let scan = archive::scan_tar(tar_path.path(), false)?;

    if redfile.is_none() {
        if let Some(plan) = detect_desktop_plan(&scan.files, &manifest.name) {
            let staging = tmp.path().join("staging");
            extract_tar(tar_path.path(), &staging)?;
            apply_desktop_layout(&staging, &manifest, &plan)?;
            return pack_staged_payload(source, opts, tmp.path(), staging, manifest, hooks, "dir");
        }
    }

    let payload_sha = sha256_file(&payload)?;
    finish_manifest(&mut manifest, compression, &scan, payload_sha, &payload)?;
    let out = output_path(source, &manifest, opts)?;
    archive::write_red(&out, &manifest, &payload, &hooks)?;
    report(&out, &manifest);
    Ok(out)
}

/// Repack a staging directory as payload and write the final .red.
fn pack_staged_payload(
    source: &Path,
    opts: &BuildOptions,
    tmp: &Path,
    staging: PathBuf,
    mut manifest: Manifest,
    hooks: BTreeMap<String, Vec<u8>>,
    _origin: &str,
) -> Result<PathBuf> {
    let payload = tmp.join("payload-staged.tar.gz");
    create_payload_from_dir(&staging, None, &payload, false)?;
    let compression = Compression::Gzip;
    let tar_path = archive::decompress_to_temp(&payload, compression, tmp)?;
    let scan = archive::scan_tar(tar_path.path(), false)?;
    let payload_sha = sha256_file(&payload)?;
    finish_manifest(&mut manifest, compression, &scan, payload_sha, &payload)?;
    let out = output_path(source, &manifest, opts)?;
    archive::write_red(&out, &manifest, &payload, &hooks)?;
    report(&out, &manifest);
    Ok(out)
}

// --- Launcher-menu integration for self-contained app packages ---

enum DesktopPlan {
    /// Loose app files without a desktop entry: relocate everything under
    /// `opt/<name>/` and generate an entry.
    Loose {
        exec_rel: String,
        icon_rel: Option<String>,
    },
    /// Portable bundle (AppDir style) whose desktop entry ships inside the app
    /// directory instead of `usr/share/applications/`. Installing such a tree
    /// as-is leaves the menu empty, so the app is relocated and its entry is
    /// re-exported into the system menu directory.
    AppDir {
        app_rel: String,
        desktop_rel: String,
    },
}

const MENU_DIR_PREFIX: &str = "usr/share/applications/";

/// Decide whether a payload needs launch-menu integration.
/// Returns None when the payload already ships its entry in the menu location.
fn detect_desktop_plan(files: &[String], name: &str) -> Option<DesktopPlan> {
    let mut stray: Vec<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|f| !f.ends_with('/') && f.ends_with(".desktop"))
        .filter(|f| !f.starts_with(MENU_DIR_PREFIX))
        .collect();
    stray.sort_unstable();
    if let Some(desktop_rel) = stray.first() {
        let app_rel = match desktop_rel.rfind('/') {
            Some(idx) => desktop_rel[..idx].to_string(),
            None => String::new(),
        };
        return Some(DesktopPlan::AppDir {
            app_rel,
            desktop_rel: (*desktop_rel).to_string(),
        });
    }
    if files.iter().any(|f| f.ends_with(".desktop")) {
        return None;
    }
    let exec_rel = pick_launcher(files, name)?;
    let icon_rel = pick_icon(files);
    Some(DesktopPlan::Loose { exec_rel, icon_rel })
}

fn pick_launcher(files: &[String], name: &str) -> Option<String> {
    let mut regular: Vec<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|f| !f.ends_with('/'))
        .collect();
    regular.sort_unstable();

    // Standard location wins.
    if let Some(found) = regular
        .iter()
        .find(|f| f.starts_with("usr/bin/") || f.starts_with("bin/"))
    {
        return Some((*found).to_string());
    }

    let lower_name = name.to_ascii_lowercase();
    let base_name = |f: &str| f.rsplit('/').next().unwrap_or(f).to_ascii_lowercase();
    let execish = |f: &str| {
        let base = base_name(f);
        base.ends_with(".sh") || base.ends_with(".appimage")
    };
    let stem_matches = |f: &str| {
        let base = base_name(f);
        let stem = base.trim_end_matches(".sh").trim_end_matches(".appimage");
        stem == lower_name
    };

    if let Some(found) = regular.iter().find(|f| stem_matches(f)) {
        return Some((*found).to_string());
    }
    // A tarball holding exactly one file is a self-contained app by itself
    // (typically a bare executable with no extension).
    if regular.len() == 1 {
        let only = regular[0];
        if execish(only) || Path::new(only).extension().is_none() {
            return Some(only.to_string());
        }
    }
    // Only top-level launcher scripts; do not pick random helper scripts deep
    // inside the tree.
    if let Some(found) = regular.iter().find(|f| !f.contains('/') && execish(f)) {
        return Some((*found).to_string());
    }
    None
}

fn pick_icon(files: &[String]) -> Option<String> {
    let mut images: Vec<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|f| {
            let lower = f.to_ascii_lowercase();
            !f.ends_with('/') && (lower.ends_with(".png") || lower.ends_with(".svg"))
        })
        .collect();
    images.sort_unstable();
    images
        .iter()
        .find(|f| f.to_ascii_lowercase().contains("icon"))
        .or_else(|| images.first())
        .map(|f| (*f).to_string())
}

fn extract_tar(tar_path: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    let file = File::open(tar_path)
        .with_context(|| format!("cannot open {}", tar_path.display()))?;
    let mut archive = Archive::new(BufReader::new(file));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Some(rel) = crate::util::sanitize_relative(&path)? else {
            continue;
        };
        let target = dst.join(&rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.unpack(&target)?;
    }
    Ok(())
}

/// Give a self-contained app payload a proper FHS layout together with a
/// desktop entry in the system menu directory.
fn apply_desktop_layout(staging: &Path, manifest: &Manifest, plan: &DesktopPlan) -> Result<()> {
    match plan {
        DesktopPlan::Loose { exec_rel, icon_rel } => {
            apply_loose_layout(staging, manifest, exec_rel, icon_rel.as_deref())?
        }
        DesktopPlan::AppDir { app_rel, desktop_rel } => {
            apply_appdir_layout(staging, manifest, app_rel, desktop_rel)?
        }
    }
    normalize_permissions(staging)?;
    Ok(())
}

/// Move every top-level entry (except `opt`) into the given directory.
fn move_children_to_opt(staging: &Path, opt_root: &Path) -> Result<()> {
    fs::create_dir_all(opt_root)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(staging)? {
        let entry = entry?;
        if entry.file_name() == "opt" {
            continue;
        }
        entries.push(entry.path());
    }
    entries.sort();
    for path in entries {
        let file_name = path.file_name().unwrap_or_default().to_os_string();
        fs::rename(&path, opt_root.join(file_name))?;
    }
    Ok(())
}

fn export_icon(staging: &Path, manifest: &Manifest, src: &Path) -> Option<String> {
    if !src.is_file() {
        return None;
    }
    let ext = match src
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
    {
        Some(ext) if ext == "svg" => "svg".to_string(),
        _ => "png".to_string(),
    };
    let icon_dir = staging.join("usr/share/icons/hicolor/512x512/apps");
    fs::create_dir_all(&icon_dir).ok()?;
    fs::copy(src, icon_dir.join(format!("{}.{}", manifest.name, ext))).ok()?;
    Some(manifest.name.clone())
}

fn apply_loose_layout(
    staging: &Path,
    manifest: &Manifest,
    plan_exec_rel: &str,
    icon_rel: Option<&str>,
) -> Result<()> {
    let structured = plan_exec_rel.starts_with("usr/") || plan_exec_rel.starts_with("opt/");
    let exec_rel = if structured {
        plan_exec_rel.to_string()
    } else {
        let opt_dir = staging.join("opt").join(&manifest.name);
        move_children_to_opt(staging, &opt_dir)?;
        format!("opt/{}/{}", manifest.name, plan_exec_rel)
    };
    let exec_abs = format!("/{exec_rel}");

    let icon_name = icon_rel.and_then(|icon_rel| {
        let icon_src = if structured {
            staging.join(icon_rel)
        } else {
            staging.join("opt").join(&manifest.name).join(icon_rel)
        };
        export_icon(staging, manifest, &icon_src)
    });

    // Prefer the launcher's own file name (e.g. "Steam++") over the sanitized
    // package name for the menu entry.
    let display_name = Path::new(plan_exec_rel)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| manifest.name.clone());
    let mut desktop = String::new();
    desktop.push_str("[Desktop Entry]\nType=Application\n");
    desktop.push_str(&format!("Name={display_name}\n"));
    if !manifest.description.is_empty() {
        desktop.push_str(&format!("Comment={}\n", manifest.description));
    }
    desktop.push_str(&format!("Exec={exec_abs}\n"));
    if let Some(icon) = &icon_name {
        desktop.push_str(&format!("Icon={icon}\n"));
    }
    desktop.push_str("Terminal=false\nCategories=Utility;\n");
    let desktop_dir = staging.join("usr/share/applications");
    fs::create_dir_all(&desktop_dir)?;
    fs::write(
        desktop_dir.join(format!("{}.desktop", manifest.name)),
        desktop,
    )?;
    Ok(())
}

/// Relocate an AppDir-style bundle under `opt/<name>/` and re-export its own
/// desktop entry into `usr/share/applications/` so the menu picks it up.
fn apply_appdir_layout(
    staging: &Path,
    manifest: &Manifest,
    app_rel: &str,
    desktop_rel: &str,
) -> Result<()> {
    let opt_root = staging.join("opt").join(&manifest.name);
    if app_rel.is_empty() {
        move_children_to_opt(staging, &opt_root)?;
    } else {
        fs::create_dir_all(staging.join("opt"))?;
        fs::rename(staging.join(app_rel), &opt_root)
            .with_context(|| format!("cannot move {app_rel} into opt/{}", manifest.name))?;
    }

    let desktop_name = Path::new(desktop_rel)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{}.desktop", manifest.name));
    let source_entry = opt_root.join(&desktop_name);
    let text = fs::read_to_string(&source_entry).unwrap_or_default();

    let icon_name = appdir_icon(staging, manifest, &opt_root, &text);
    let exec_abs = appdir_exec(&opt_root, &manifest.name, &text);

    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("TryExec=") {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("Exec=") {
            let (_, args) = split_exec(value);
            let exec = exec_abs.clone().unwrap_or_else(|| split_exec(value).0);
            if args.is_empty() {
                out.push_str(&format!("Exec={exec}\n"));
            } else {
                out.push_str(&format!("Exec={exec} {args}\n"));
            }
            continue;
        }
        if trimmed.starts_with("Icon=") {
            if let Some(icon) = &icon_name {
                out.push_str(&format!("Icon={icon}\n"));
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if !out.contains("Exec=") {
        out.push_str(&format!(
            "[Desktop Entry]\nType=Application\nName={}\nExec={}\nTerminal=false\nCategories=Utility;\n",
            manifest.name,
            exec_abs.unwrap_or_else(|| format!("/opt/{}/AppRun", manifest.name))
        ));
    }

    let desktop_dir = staging.join("usr/share/applications");
    fs::create_dir_all(&desktop_dir)?;
    fs::write(desktop_dir.join(format!("{}.desktop", manifest.name)), out)?;
    Ok(())
}

fn appdir_icon(staging: &Path, manifest: &Manifest, opt_root: &Path, text: &str) -> Option<String> {
    let mut candidates = vec![opt_root.join(".DirIcon")];
    if let Some(icon_value) = text
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("Icon="))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        for ext in ["png", "svg", "xpm"] {
            candidates.push(opt_root.join(format!("{icon_value}.{ext}")));
        }
    }
    candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| export_icon(staging, manifest, candidate))
}

/// Build an absolute Exec path for the relocated AppDir. AppRun is the portable
/// entry point; otherwise a relative command from the shipped entry is mapped.
fn appdir_exec(opt_root: &Path, name: &str, text: &str) -> Option<String> {
    if opt_root.join("AppRun").is_file() {
        return Some(format!("/opt/{name}/AppRun"));
    }
    for line in text.lines() {
        let Some(value) = line.trim_start().strip_prefix("Exec=") else {
            continue;
        };
        let command = split_exec(value).0;
        let relative = command.trim_start_matches("./");
        if relative.is_empty() || relative.starts_with('/') {
            break;
        }
        if opt_root.join(relative).is_file() {
            return Some(format!("/opt/{name}/{relative}"));
        }
    }
    None
}

/// Split an Exec value into its command and remaining arguments.
fn split_exec(value: &str) -> (String, String) {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[..end].to_string(), rest[end + 1..].trim().to_string());
        }
    }
    match value.split_once(char::is_whitespace) {
        Some((command, args)) => (command.to_string(), args.trim().to_string()),
        None => (value.to_string(), String::new()),
    }
}

/// Upstream tarballs often ship 0700 modes; make everything world-readable so
/// the installed app works for normal users.
#[cfg(unix)]
fn normalize_permissions(staging: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for entry in WalkDir::new(staging).follow_links(false) {
        let entry = entry?;
        let meta = fs::symlink_metadata(entry.path())?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let mode = meta.permissions().mode();
        let target = if meta.is_dir() || mode & 0o111 != 0 {
            0o755
        } else {
            0o644
        };
        if mode & 0o777 != target {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(target))?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn normalize_permissions(_staging: &Path) -> Result<()> {
    Ok(())
}

/// Build a native pacman package (`.pkg.tar.gz`) from a tar archive or directory.
fn build_native(source: &Path, opts: &BuildOptions) -> Result<PathBuf> {
    println!("==> Building pacman package from {}", source.display());
    let kind = detect_kind(source)?;
    if !matches!(kind, Kind::Tar | Kind::Dir) {
        bail!(
            "--native only accepts tar archives or directories; convert {} with 'red -rebuild' first",
            source.display()
        );
    }
    let redfile = load_redfile(source, opts)?;
    let origin = if kind == Kind::Dir { "dir" } else { "tar" };
    let mut manifest = make_manifest(source, opts, redfile.as_ref(), origin)?;
    let tmp = tempfile::tempdir()?;
    let tar_path = match kind {
        Kind::Tar => {
            let compression = archive::tar_compression(source)?;
            archive::decompress_to_temp(source, compression, tmp.path())?
        }
        Kind::Dir => {
            let payload = tmp.path().join("payload.tar.gz");
            create_payload_from_dir(source, redfile.as_ref(), &payload, true)?;
            archive::decompress_to_temp(&payload, Compression::Gzip, tmp.path())?
        }
        _ => unreachable!(),
    };
    let staging = tmp.path().join("staging");
    extract_tar(tar_path.path(), &staging)?;
    if redfile.is_none() {
        let files = list_rel_files(&staging)?;
        if let Some(plan) = detect_desktop_plan(&files, &manifest.name) {
            apply_desktop_layout(&staging, &manifest, &plan)?;
        }
    }
    if manifest.arch == "any" {
        manifest.arch = std::env::consts::ARCH.to_string();
    }
    write_pacman_pkg(&staging, &manifest, source, opts)
}

fn list_rel_files(staging: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(staging).follow_links(false).sort_by_file_name() {
        let entry = entry?;
        let rel = entry.path().strip_prefix(staging)?;
        if rel.as_os_str().is_empty() || entry.file_type().is_dir() {
            continue;
        }
        files.push(rel.to_string_lossy().replace('\\', "/"));
    }
    Ok(files)
}

fn write_pacman_pkg(
    staging: &Path,
    manifest: &Manifest,
    source: &Path,
    opts: &BuildOptions,
) -> Result<PathBuf> {
    let lower = manifest.name.to_ascii_lowercase();
    if manifest.name != lower {
        bail!(
            "pacman package names must be lowercase: '{}' (use --name {})",
            manifest.name,
            lower
        );
    }
    let pkgver = format!("{}-{}", manifest.version.replace('-', "_"), manifest.release);

    let mut installed_size = 0u64;
    for entry in WalkDir::new(staging).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            installed_size = installed_size.saturating_add(entry.metadata()?.len());
        }
    }

    let mut pkginfo = String::new();
    pkginfo.push_str(&format!("pkgname = {}\n", manifest.name));
    pkginfo.push_str(&format!("pkgver = {pkgver}\n"));
    if !manifest.description.is_empty() {
        pkginfo.push_str(&format!("pkgdesc = {}\n", manifest.description));
    }
    if !manifest.url.is_empty() {
        pkginfo.push_str(&format!("url = {}\n", manifest.url));
    }
    pkginfo.push_str(&format!("builddate = {}\n", now_unix()));
    pkginfo.push_str(&format!("size = {installed_size}\n"));
    pkginfo.push_str(&format!("arch = {}\n", manifest.arch));
    for license in &manifest.license {
        pkginfo.push_str(&format!("license = {license}\n"));
    }
    for dep in &manifest.depends {
        pkginfo.push_str(&format!("depend = {dep}\n"));
    }
    for conflict in &manifest.conflicts {
        pkginfo.push_str(&format!("conflict = {conflict}\n"));
    }
    for provide in &manifest.provides {
        pkginfo.push_str(&format!("provides = {provide}\n"));
    }

    let out = match &opts.out {
        Some(path) => path.clone(),
        None => {
            let dir = if source.is_dir() {
                source.to_path_buf()
            } else {
                source
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."))
            };
            dir.join(format!(
                "{}-{}-{}.pkg.tar.gz",
                manifest.name, pkgver, manifest.arch
            ))
        }
    };

    let file =
        File::create(&out).with_context(|| format!("cannot create {}", out.display()))?;
    let encoder = GzEncoder::new(file, GzCompression::default());
    let mut builder = tar::Builder::new(encoder);

    let mut header = tar::Header::new_gnu();
    header.set_size(pkginfo.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(now_unix());
    header.set_cksum();
    builder.append_data(&mut header, ".PKGINFO", pkginfo.as_bytes())?;

    for entry in WalkDir::new(staging).follow_links(false).sort_by_file_name() {
        let entry = entry?;
        let rel = entry.path().strip_prefix(staging)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        if entry.file_type().is_dir() {
            builder.append_dir(rel, entry.path())?;
        } else if entry.file_type().is_file() || entry.file_type().is_symlink() {
            builder.append_path_with_name(entry.path(), rel)?;
        }
    }
    let encoder = builder.into_inner()?;
    let mut file = encoder.finish()?;
    file.flush()?;
    file.sync_all()?;

    println!(
        "==> Created {} (pacman package, {} installed)",
        out.display(),
        crate::util::human_size(installed_size)
    );
    Ok(out)
}
fn create_payload_from_dir(
    source: &Path,
    redfile: Option<&Redfile>,
    output: &Path,
    skip_metadata: bool,
) -> Result<()> {
    let payload_root = match redfile.and_then(|r| r.payload_dir.as_ref()) {
        Some(dir) => {
            let path = source.join(dir);
            if !path.is_dir() {
                bail!("payload_dir does not exist: {}", path.display());
            }
            path
        }
        None => source.to_path_buf(),
    };

    let mut exclude: BTreeSet<String> = BTreeSet::new();
    if let Some(redfile) = redfile {
        for item in &redfile.exclude {
            exclude.insert(item.trim_start_matches("./").trim_end_matches('/').to_string());
        }
    }

    let file = File::create(output)?;
    let mut builder = tar::Builder::new(GzEncoder::new(file, GzCompression::default()));
    for entry in WalkDir::new(&payload_root)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry?;
        let rel = entry.path().strip_prefix(&payload_root)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if is_excluded(&rel_str, &exclude) {
            continue;
        }
        if skip_metadata && is_build_metadata(&rel_str) {
            continue;
        }
        if entry.file_type().is_dir() {
            builder.append_dir(rel, entry.path())?;
        } else if entry.file_type().is_file() || entry.file_type().is_symlink() {
            builder.append_path_with_name(entry.path(), rel)?;
        }
    }
    let encoder = builder.into_inner()?;
    let mut file = encoder.finish()?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn is_build_metadata(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or(rel);
    matches!(first, "Redfile" | "Redfile.json" | "hooks" | ".git" | ".hg" | ".svn")
        || (rel.ends_with(".red") && !rel.contains('/'))
}

fn is_excluded(rel: &str, patterns: &BTreeSet<String>) -> bool {
    patterns
        .iter()
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| rel == pattern || rel.starts_with(&format!("{pattern}/")))
}

fn build_arch(source: &Path, opts: &BuildOptions, origin: &str) -> Result<PathBuf> {
    println!("==> Converting {} package {}", origin, source.display());
    let compression = archive::tar_compression(source)?;
    let tmp = tempfile::tempdir()?;
    let tar_path = archive::decompress_to_temp(source, compression, tmp.path())?;
    let scan = archive::scan_tar(tar_path.path(), true)?;
    let (info, install_script) = read_arch_metadata(tar_path.path())?;

    let mut manifest = make_manifest(source, opts, None, origin)?;
    if let Some(value) = info.first("pkgname") {
        if opts.name.is_none() {
            manifest.name = value.to_string();
        }
    }
    if let Some(value) = info.first("pkgver") {
        if opts.version.is_none() {
            let (version, release) = split_arch_version(value);
            manifest.version = version;
            if opts.release.is_none() {
                manifest.release = release;
            }
        }
    }
    if let Some(value) = info.first("pkgdesc") {
        if opts.description.is_none() {
            manifest.description = value.to_string();
        }
    }
    if let Some(value) = info.first("url") {
        if opts.url.is_none() {
            manifest.url = value.to_string();
        }
    }
    if let Some(value) = info.first("arch") {
        if opts.arch.is_none() {
            manifest.arch = value.to_string();
        }
    }
    if let Some(value) = info.first("packager") {
        if opts.maintainer.is_none() {
            manifest.maintainer = value.to_string();
        }
    }
    if opts.license.is_empty() {
        manifest.license = info.all("license");
    }
    if opts.depends.is_empty() {
        manifest.depends = info.all("depend");
        manifest.opt_depends = info.all("optdepend");
    }
    if opts.provides.is_empty() {
        manifest.provides = info.all("provides");
    }
    if opts.conflicts.is_empty() {
        manifest.conflicts = info.all("conflict");
    }
    if opts.replaces.is_empty() {
        manifest.replaces = info.all("replaces");
    }

    let payload_sha = sha256_file(source)?;
    let mut hooks = BTreeMap::new();
    if let Some(script) = install_script {
        let member = "hooks/arch_install.sh".to_string();
        manifest
            .hooks
            .insert("arch_install".to_string(), member.clone());
        hooks.insert(member, script);
    }
    finish_manifest(&mut manifest, compression, &scan, payload_sha, source)?;
    let out = output_path(source, &manifest, opts)?;
    archive::write_red(&out, &manifest, source, &hooks)?;
    report(&out, &manifest);
    Ok(out)
}

fn build_deb(source: &Path, opts: &BuildOptions) -> Result<PathBuf> {
    println!("==> Converting DEB package {} with dpkg-deb", source.display());
    ensure_tool("dpkg-deb", "install the Debian package manager (dpkg)")?;

    let tmp = tempfile::tempdir()?;
    let control_dir = tmp.path().join("control");
    fs::create_dir_all(&control_dir)?;
    let status = Command::new("dpkg-deb")
        .arg("-e")
        .arg(source)
        .arg(&control_dir)
        .status()
        .context("cannot run dpkg-deb -e")?;
    if !status.success() {
        bail!("dpkg-deb failed to extract control data from {}", source.display());
    }

    let control_text = fs::read_to_string(control_dir.join("control"))
        .context("DEB control data contains no control file")?;
    let control = parse_deb_control(&control_text);
    let mut scripts = BTreeMap::new();
    for name in ["preinst", "postinst", "prerm", "postrm"] {
        let path = control_dir.join(name);
        if path.is_file() {
            scripts.insert(name.to_string(), fs::read(&path)?);
        }
    }

    let payload = tmp.path().join("payload.tar");
    let data_out = File::create(&payload)?;
    let status = Command::new("dpkg-deb")
        .arg("--fsys-tarfile")
        .arg(source)
        .stdout(Stdio::from(data_out))
        .status()
        .context("cannot run dpkg-deb --fsys-tarfile")?;
    if !status.success() {
        bail!("dpkg-deb failed to extract data from {}", source.display());
    }
    let scan = archive::scan_tar(&payload, false)?;

    let mut manifest = make_manifest(source, opts, None, "deb")?;
    manifest.compression = "none".to_string();

    let field = |key: &str| control.get(key).cloned().unwrap_or_default();
    if opts.name.is_none() && !field("Package").is_empty() {
        manifest.name = field("Package");
    }
    if opts.version.is_none() && !field("Version").is_empty() {
        manifest.version = field("Version");
    }
    if opts.description.is_none() {
        let desc = field("Description");
        if !desc.is_empty() {
            manifest.description = desc;
        }
    }
    if opts.maintainer.is_none() {
        manifest.maintainer = field("Maintainer");
    }
    if opts.url.is_none() {
        manifest.url = field("Homepage");
    }
    if opts.arch.is_none() {
        let arch = field("Architecture");
        if !arch.is_empty() {
            manifest.arch = map_foreign_arch(&arch);
        }
    }
    manifest.foreign_depends = split_list(&field("Depends"));
    if manifest.foreign_depends.is_empty() {
        manifest.foreign_depends = split_list(&field("Pre-Depends"));
    }
    manifest.foreign_conflicts = split_list(&field("Conflicts"));
    manifest.foreign_conflicts.extend(split_list(&field("Breaks")));

    let mut hooks = BTreeMap::new();
    for (deb_name, phase) in [
        ("preinst", "pre_install"),
        ("postinst", "post_install"),
        ("prerm", "pre_remove"),
        ("postrm", "post_remove"),
    ] {
        if let Some(data) = scripts.get(deb_name) {
            let member = format!("hooks/{phase}.sh");
            manifest.hooks.insert(phase.to_string(), member.clone());
            hooks.insert(member, data.clone());
        }
    }

    let payload_sha = sha256_file(&payload)?;
    finish_manifest(&mut manifest, Compression::None, &scan, payload_sha, &payload)?;
    let out = output_path(source, &manifest, opts)?;
    archive::write_red(&out, &manifest, &payload, &hooks)?;
    if !manifest.foreign_depends.is_empty() {
        println!(
            "==> Note: DEB dependencies are recorded as informational (foreign_depends); they are not enforced"
        );
    }
    report(&out, &manifest);
    Ok(out)
}

fn build_rpm(source: &Path, opts: &BuildOptions) -> Result<PathBuf> {
    println!("==> Converting RPM package {} with rpm2cpio", source.display());
    ensure_tool("rpm", "install the RPM package manager")?;
    ensure_tool("rpm2cpio", "install rpm2cpio (part of rpm-tools)")?;
    ensure_tool("cpio", "install cpio")?;

    let meta = rpm_query(
        source,
        "%{NAME}\n%{VERSION}\n%{RELEASE}\n%{ARCH}\n%{SUMMARY}\n%{URL}\n%{LICENSE}\n%{PACKAGER}\n%{EPOCH}",
    )?;
    let meta: Vec<&str> = meta.lines().map(str::trim).collect();
    let get = |index: usize| meta.get(index).copied().unwrap_or("").trim();
    let clean = |value: &str| {
        if value == "(none)" || value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    };

    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("root");
    fs::create_dir_all(&root)?;
    let status = Command::new("bash")
        .arg("-c")
        .arg("set -o pipefail; rpm2cpio \"$1\" | cpio -idm --quiet")
        .arg("red-rpm")
        .arg(source)
        .current_dir(&root)
        .status()
        .context("cannot run rpm2cpio/cpio")?;
    if !status.success() {
        bail!("rpm2cpio/cpio failed to extract {}", source.display());
    }

    let payload = tmp.path().join("payload.tar.gz");
    create_payload_from_dir(&root, None, &payload, false)?;
    let scan = archive::scan_tar(
        archive::decompress_to_temp(&payload, Compression::Gzip, tmp.path())?.path(),
        false,
    )?;

    let mut manifest = make_manifest(source, opts, None, "rpm")?;
    if opts.name.is_none() {
        if let Some(name) = clean(get(0)) {
            manifest.name = name;
        }
    }
    if opts.version.is_none() {
        if let Some(mut version) = clean(get(1)) {
            if let Some(epoch) = clean(get(8)) {
                version = format!("{epoch}:{version}");
            }
            if let Some(release) = clean(get(2)) {
                match release.parse::<u32>() {
                    Ok(n) if n > 0 && opts.release.is_none() => manifest.release = n,
                    _ => version = format!("{version}-{release}"),
                }
            }
            manifest.version = version;
        }
    }
    if opts.description.is_none() {
        if let Some(value) = clean(get(4)) {
            manifest.description = value;
        }
    }
    if opts.url.is_none() {
        if let Some(value) = clean(get(5)) {
            manifest.url = value;
        }
    }
    if opts.maintainer.is_none() {
        if let Some(value) = clean(get(7)) {
            manifest.maintainer = value;
        }
    }
    if opts.arch.is_none() {
        if let Some(value) = clean(get(3)) {
            manifest.arch = map_foreign_arch(&value);
        }
    }
    if opts.license.is_empty() {
        if let Some(value) = clean(get(6)) {
            manifest.license = vec![value];
        }
    }
    if opts.depends.is_empty() {
        manifest.foreign_depends = rpm_query_list(source, "--requires")?;
    }
    if opts.provides.is_empty() {
        manifest.provides = rpm_query_list(source, "--provides")?;
    }
    if opts.conflicts.is_empty() {
        manifest.foreign_conflicts = rpm_query_list(source, "--conflicts")?;
    }
    if opts.replaces.is_empty() {
        manifest.replaces = rpm_query_list(source, "--obsoletes")?;
    }

    let payload_sha = sha256_file(&payload)?;
    finish_manifest(&mut manifest, Compression::Gzip, &scan, payload_sha, &payload)?;
    let out = output_path(source, &manifest, opts)?;
    archive::write_red(&out, &manifest, &payload, &BTreeMap::new())?;
    if !manifest.foreign_depends.is_empty() {
        println!(
            "==> Note: RPM dependencies are recorded as informational (foreign_depends); they are not enforced"
        );
    }
    report(&out, &manifest);
    Ok(out)
}

fn ensure_tool(tool: &str, hint: &str) -> Result<()> {
    match Command::new(tool).arg("--version").output() {
        Ok(_) => Ok(()),
        Err(_) => bail!("{tool} not found; {hint}"),
    }
}

fn rpm_query(source: &Path, format: &str) -> Result<String> {
    let output = Command::new("rpm")
        .args(["-qp", "--qf", format])
        .arg(source)
        .output()
        .context("cannot run rpm -qp")?;
    if !output.status.success() {
        bail!(
            "rpm failed to read {}: {}",
            source.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn rpm_query_list(source: &Path, flag: &str) -> Result<Vec<String>> {
    let output = Command::new("rpm")
        .args(["-qp", flag])
        .arg(source)
        .output()
        .with_context(|| format!("cannot run rpm -qp {flag}"))?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("rpmlib("))
        .map(str::to_string)
        .collect())
}

fn build_aur(source: &Path, opts: &BuildOptions) -> Result<Vec<PathBuf>> {
    println!("==> Building AUR package with makepkg in {}", source.display());
    ensure_makepkg()?;
    let status = Command::new("makepkg")
        .args(["-f", "--noconfirm"])
        .current_dir(source)
        .status()
        .context("cannot run makepkg")?;
    if !status.success() {
        bail!("makepkg failed with status {status}");
    }
    let output = Command::new("makepkg")
        .args(["--packagelist"])
        .current_dir(source)
        .output()
        .context("cannot run makepkg --packagelist")?;
    if !output.status.success() {
        bail!("makepkg --packagelist failed");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut packages: Vec<PathBuf> = text
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(|line| {
            let path = PathBuf::from(line);
            if path.is_absolute() {
                path
            } else {
                source.join(path)
            }
        })
        .filter(|path| path.exists())
        .collect();
    packages.sort();
    packages.dedup();
    if packages.is_empty() {
        bail!("makepkg produced no package files");
    }
    if packages.len() > 1 && opts.out.is_some() {
        bail!("this PKGBUILD produces {} packages; --out cannot be used", packages.len());
    }
    let mut outputs = Vec::new();
    for package in packages {
        outputs.push(build_arch(&package, opts, "aur")?);
    }
    Ok(outputs)
}

fn ensure_makepkg() -> Result<()> {
    match Command::new("makepkg").arg("--version").output() {
        Ok(output) if output.status.success() => Ok(()),
        _ => bail!(
            "makepkg not found. Install base-devel (Arch) or build the package first and pass the .pkg.tar.* file"
        ),
    }
}

fn load_redfile(source: &Path, opts: &BuildOptions) -> Result<Option<Redfile>> {
    if let Some(path) = &opts.redfile {
        return Ok(Some(Redfile::load(path)?));
    }
    let candidates = if source.is_dir() {
        vec![source.join("Redfile"), source.join("Redfile.json")]
    } else {
        let parent = source.parent().unwrap_or(Path::new("."));
        vec![parent.join("Redfile"), parent.join("Redfile.json")]
    };
    for candidate in candidates {
        if candidate.exists() {
            return Ok(Some(Redfile::load(&candidate)?));
        }
    }
    Ok(None)
}

fn make_manifest(
    source: &Path,
    opts: &BuildOptions,
    redfile: Option<&Redfile>,
    origin: &str,
) -> Result<Manifest> {
    let mut manifest = Manifest::default();
    if let Some(redfile) = redfile {
        if let Some(value) = &redfile.name {
            manifest.name = value.clone();
        }
        if let Some(value) = &redfile.version {
            manifest.version = value.clone();
        }
        if let Some(value) = redfile.release {
            manifest.release = value;
        }
        if let Some(value) = &redfile.arch {
            manifest.arch = value.clone();
        }
        if let Some(value) = &redfile.description {
            manifest.description = value.clone();
        }
        if let Some(value) = &redfile.url {
            manifest.url = value.clone();
        }
        if !redfile.license.is_empty() {
            manifest.license = redfile.license.clone();
        }
        if let Some(value) = &redfile.maintainer {
            manifest.maintainer = value.clone();
        }
        manifest.depends = redfile.depends.clone();
        manifest.opt_depends = redfile.opt_depends.clone();
        manifest.provides = redfile.provides.clone();
        manifest.conflicts = redfile.conflicts.clone();
        manifest.replaces = redfile.replaces.clone();
    }

    if let Some(value) = &opts.name {
        manifest.name = value.clone();
    }
    if let Some(value) = &opts.version {
        manifest.version = value.clone();
    }
    if let Some(value) = opts.release {
        manifest.release = value;
    }
    if let Some(value) = &opts.arch {
        manifest.arch = value.clone();
    }
    if let Some(value) = &opts.description {
        manifest.description = value.clone();
    }
    if let Some(value) = &opts.url {
        manifest.url = value.clone();
    }
    if !opts.license.is_empty() {
        manifest.license = opts.license.clone();
    }
    if let Some(value) = &opts.maintainer {
        manifest.maintainer = value.clone();
    }
    if !opts.depends.is_empty() {
        manifest.depends = opts.depends.clone();
    }
    if !opts.provides.is_empty() {
        manifest.provides = opts.provides.clone();
    }
    if !opts.conflicts.is_empty() {
        manifest.conflicts = opts.conflicts.clone();
    }
    if !opts.replaces.is_empty() {
        manifest.replaces = opts.replaces.clone();
    }

    if manifest.name.is_empty() || manifest.version.is_empty() {
        let (name, version, arch, release) = infer_from_filename(source);
        if manifest.name.is_empty() {
            if let Some(name) = name {
                manifest.name = name;
            } else if source.is_dir() {
                manifest.name = source
                    .file_name()
                    .map(|s| sanitize_name(&s.to_string_lossy()))
                    .unwrap_or_default();
            }
        }
        if manifest.version.is_empty() {
            if let Some(version) = version {
                manifest.version = version;
            }
        }
        if manifest.arch == "any" {
            if let Some(arch) = arch {
                manifest.arch = arch;
            }
        }
        if manifest.release == 1 {
            if let Some(release) = release {
                manifest.release = release;
            }
        }
    }

    if manifest.version.is_empty() && (origin == "tar" || origin == "dir") {
        eprintln!("red: warning: package version could not be inferred; using 0");
        manifest.version = "0".to_string();
    }
    if manifest.name.is_empty() {
        bail!("cannot determine package name; use --name or provide a Redfile");
    }
    validate_package_name(&manifest.name)?;
    manifest.origin = origin.to_string();
    manifest.built_at = now_unix();
    manifest.red_version = env!("CARGO_PKG_VERSION").to_string();
    Ok(manifest)
}

fn attach_hooks(
    manifest: &mut Manifest,
    source: &Path,
    redfile: Option<&Redfile>,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut hooks = BTreeMap::new();
    let Some(redfile) = redfile else {
        return Ok(hooks);
    };
    let base = if source.is_dir() {
        source.to_path_buf()
    } else {
        source.parent().unwrap_or(Path::new(".")).to_path_buf()
    };
    for (phase, rel) in &redfile.hooks {
        let phase = crate::manifest::normalize_hook_phase(phase);
        if !valid_hook_phases().contains(&phase.as_str()) {
            bail!(
                "unknown hook phase {phase} (expected one of {})",
                valid_hook_phases().join(", ")
            );
        }
        let path = base.join(rel);
        let data = fs::read(&path)
            .with_context(|| format!("cannot read hook {} for phase {phase}", path.display()))?;
        let member = format!("hooks/{phase}.sh");
        manifest.hooks.insert(phase.clone(), member.clone());
        hooks.insert(member, data);
    }
    Ok(hooks)
}

fn finish_manifest(
    manifest: &mut Manifest,
    compression: Compression,
    scan: &ScanResult,
    payload_sha: String,
    payload_path: &Path,
) -> Result<()> {
    manifest.format = crate::manifest::FORMAT_VERSION;
    manifest.payload = compression.payload_name();
    manifest.compression = compression.name().to_string();
    manifest.payload_size = fs::metadata(payload_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    manifest.installed_size = scan.installed_size;
    manifest.sha256 = payload_sha;
    manifest.files = scan.files.clone();
    manifest.validate()?;
    Ok(())
}

fn output_path(source: &Path, manifest: &Manifest, opts: &BuildOptions) -> Result<PathBuf> {
    if let Some(out) = &opts.out {
        return Ok(out.clone());
    }
    let dir = if source.is_dir() {
        source.parent().unwrap_or(Path::new("."))
    } else {
        source.parent().unwrap_or(Path::new("."))
    };
    let version = manifest.full_version().replace('/', "_");
    Ok(dir.join(format!(
        "{}-{}-{}.red",
        manifest.name, version, manifest.arch
    )))
}

fn report(out: &Path, manifest: &Manifest) {
    println!(
        "==> Created {} ({} package, {} installed, payload {})",
        out.display(),
        manifest.name,
        crate::util::human_size(manifest.installed_size),
        crate::util::human_size(manifest.payload_size)
    );
}

fn infer_from_filename(path: &Path) -> (Option<String>, Option<String>, Option<String>, Option<u32>) {
    let stem = match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => return (None, None, None, None),
    };
    let mut stem = stem;
    for suffix in [
        ".pkg.tar.zst",
        ".pkg.tar.xz",
        ".pkg.tar.gz",
        ".pkg.tar",
        ".tar.zst",
        ".tar.xz",
        ".tar.gz",
        ".tar.bz2",
        ".tgz",
        ".txz",
        ".tzst",
        ".tar",
        ".deb",
        ".rpm",
    ] {
        if stem.to_ascii_lowercase().ends_with(suffix) {
            stem.truncate(stem.len() - suffix.len());
            break;
        }
    }

    // "-", "_" and whitespace all act as field separators in release file names
    // such as "Steam++_v3.1.0_linux_x64.tgz".
    let mut parts: Vec<String> = stem
        .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect();

    let mut arch = None;
    // "x86_64" is split into ["x86", "64"] by the "_" separator, so try the
    // last two tokens joined before falling back to a single token.
    if parts.len() >= 2 {
        let joined = format!("{}_{}", parts[parts.len() - 2], parts[parts.len() - 1]);
        if let Some(canonical) = canonical_arch(&joined) {
            arch = Some(canonical.to_string());
            parts.pop();
            parts.pop();
        }
    }
    if arch.is_none() {
        if let Some(last) = parts.last() {
            if let Some(canonical) = canonical_arch(last) {
                arch = Some(canonical.to_string());
                parts.pop();
            }
        }
    }
    // Target-triple filler can repeat, e.g. `x86_64-unknown-linux-gnu`.
    let mut popped = 0;
    while popped < 3 && parts.len() > 1 {
        match parts.last() {
            Some(last) if is_platform_token(last) => {
                parts.pop();
                popped += 1;
            }
            _ => break,
        }
    }
    let mut release = None;
    if parts.len() >= 3 {
        if let Some(last) = parts.last() {
            if last.len() == 1 && last.chars().all(|c| c.is_ascii_digit()) {
                release = last.parse::<u32>().ok();
                parts.pop();
            }
        }
    }
    let mut version = None;
    if parts.len() >= 2 {
        if let Some(last) = parts.last() {
            if let Some(parsed) = version_token(last) {
                version = Some(parsed);
                parts.pop();
            }
        }
    }
    let name = if parts.is_empty() {
        None
    } else {
        match sanitize_name(&parts.join("-")) {
            name if name.is_empty() => None,
            name => Some(name),
        }
    };
    (name, version, arch, release)
}

fn version_token(value: &str) -> Option<String> {
    let trimmed = value
        .strip_prefix('v')
        .or_else(|| value.strip_prefix('V'))
        .unwrap_or(value);
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if !first.is_ascii_digit() {
        return None;
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '~' | ':'))
    {
        return None;
    }
    Some(trimmed.to_string())
}

fn canonical_arch(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "x86_64" | "x86-64" | "amd64" | "x64" => Some("x86_64"),
        "aarch64" | "arm64" => Some("aarch64"),
        "i686" | "i386" | "x86" => Some("i686"),
        "armv7h" | "armv7l" | "armv7" => Some("armv7h"),
        "any" | "all" | "noarch" => Some("any"),
        _ => None,
    }
}

fn is_platform_token(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "linux"
            | "linux64"
            | "windows"
            | "win"
            | "win64"
            | "macos"
            | "osx"
            | "darwin"
            // Rust/Nix target-triple filler such as `x86_64-unknown-linux-gnu`.
            | "unknown"
            | "gnu"
            | "musl"
            | "pc"
    )
}

/// Lower-case a derived name and drop characters the package name validator
/// rejects, so files like `Steam  _v3.1.0_linux_x64.tgz` still build.
pub(crate) fn sanitize_name(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '+' | '-') {
            if c == '-' && (out.is_empty() || out.ends_with('-')) {
                continue;
            }
            out.push(c);
        }
    }
    out.trim_matches(|c: char| matches!(c, '-' | '.' | '_'))
        .to_string()
}

fn split_arch_version(value: &str) -> (String, u32) {
    if let Some((version, release)) = value.rsplit_once('-') {
        if !release.is_empty() && release.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(release) = release.parse::<u32>() {
                return (version.to_string(), release);
            }
        }
    }
    (value.to_string(), 1)
}

struct ArchInfo {
    fields: BTreeMap<String, Vec<String>>,
}

impl ArchInfo {
    fn first(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(|v| v.first()).map(|s| s.as_str())
    }
    fn all(&self, key: &str) -> Vec<String> {
        self.fields.get(key).cloned().unwrap_or_default()
    }
}

fn read_arch_metadata(tar_path: &Path) -> Result<(ArchInfo, Option<Vec<u8>>)> {
    let file = File::open(tar_path)?;
    let mut archive = Archive::new(BufReader::new(file));
    let mut info: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut install: Option<Vec<u8>> = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw = entry.path()?.to_string_lossy().into_owned();
        let name = raw.trim_start_matches("./").to_string();
        if name == ".PKGINFO" {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    info.entry(key.trim().to_string())
                        .or_default()
                        .push(value.trim().to_string());
                }
            }
        } else if name == ".INSTALL" {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            install = Some(data);
        }
    }
    if !info.contains_key("pkgname") {
        bail!("{} does not contain .PKGINFO", tar_path.display());
    }
    Ok((ArchInfo { fields: info }, install))
}

fn parse_deb_control(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if line.is_empty() {
            current = None;
        } else if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(key) = &current {
                let value = map.entry(key.clone()).or_insert_with(String::new);
                if !value.is_empty() {
                    value.push('\n');
                }
                value.push_str(line.trim());
            }
        } else if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            map.insert(key.clone(), value.trim().to_string());
            current = Some(key);
        }
    }
    map
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn map_foreign_arch(arch: &str) -> String {
    match arch {
        "amd64" => "x86_64",
        "arm64" => "aarch64",
        "i386" | "i686" => "i686",
        "armhf" | "armv7l" => "armv7h",
        "all" | "noarch" => "any",
        other => other,
    }
    .to_string()
}

