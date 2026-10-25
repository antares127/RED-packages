use crate::archive::{self, read_manifest, read_red};
use crate::db::{
    build_record, find_repo_by_name, find_repo_for_dep, scan_repos, Database, FileEntry,
    InstalledRecord,
};
use crate::manifest::Manifest;
use crate::util::{
    human_size, is_arch_metadata, print_field, prune_empty_parents, remove_file_if_exists,
    sanitize_relative,
};
use crate::version::Constraint;
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::BufReader;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tar::Archive;
use tempfile::NamedTempFile;

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub root: PathBuf,
    pub nodeps: bool,
    pub force: bool,
    pub run_hooks: bool,
}

impl InstallOptions {
    pub fn hooks_enabled(&self) -> bool {
        self.run_hooks
    }
}

struct InstallCtx {
    db: Database,
    opts: InstallOptions,
    stack: Vec<String>,
}

pub fn install(root: &Path, target: &str, opts: &InstallOptions) -> Result<()> {
    let db = Database::load(root)?;
    let mut ctx = InstallCtx {
        db,
        opts: opts.clone(),
        stack: Vec::new(),
    };
    let path = resolve_target(root, target)?;
    if is_bundle_path(&path) {
        return ctx.install_bundle(&path);
    }
    ctx.install_file(&path)
}

pub fn is_bundle_path(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .map(|ext| ext.eq_ignore_ascii_case("ared"))
            .unwrap_or(false)
}

fn resolve_target(root: &Path, target: &str) -> Result<PathBuf> {
    let as_path = PathBuf::from(target);
    if as_path.is_file() {
        return Ok(as_path);
    }
    if target.ends_with(".red") || target.ends_with(".ared") {
        bail!("no such file: {target}");
    }
    if let Some(entry) = find_repo_by_name(root, target)? {
        return Ok(entry.path);
    }
    bail!(
        "package '{target}' not found (not an installed package or a .red file, and no repository provides it)"
    )
}

impl InstallCtx {
    /// Install every `.red` package inside a `.ared` bundle. Failures are
    /// collected so one broken package does not stop the rest of the bundle.
    fn install_bundle(&mut self, path: &Path) -> Result<()> {
        let bundle = crate::bundle::read_bundle(path)?;
        println!(
            "==> Installing bundle {} ({} packages, {})",
            bundle.name,
            bundle.packages.len(),
            human_size(bundle.total_size())
        );

        let tmp = tempfile::tempdir().context("cannot create bundle workspace")?;
        let members = crate::bundle::extract_bundle(path, tmp.path())?;
        if members.is_empty() {
            bail!("no .red packages inside {}", path.display());
        }

        let total = members.len();
        let mut installed = 0usize;
        let mut skipped = 0usize;
        let mut failed: Vec<String> = Vec::new();
        for (index, member) in members.iter().enumerate() {
            let label = member
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| member.display().to_string());
            println!("==> [{}/{}] {}", index + 1, total, label);

            // Already-installed packages are skipped instead of aborting the
            // whole bundle (unless --force was given).
            match read_manifest(member) {
                Ok(manifest) => {
                    if let Some(existing) = self.db.packages.get(&manifest.name) {
                        if existing.manifest.full_version() == manifest.full_version()
                            && !self.opts.force
                        {
                            println!(
                                "==> {} {} is already installed, skipping",
                                manifest.name,
                                manifest.full_version()
                            );
                            skipped += 1;
                            continue;
                        }
                    }
                }
                Err(err) => {
                    eprintln!("red: error: cannot read {label}: {err:#}");
                    failed.push(label.clone());
                    continue;
                }
            }

            match self.install_file(member) {
                Ok(()) => installed += 1,
                Err(err) => {
                    eprintln!("red: error: {err:#}");
                    failed.push(label);
                }
            }
        }

        run_desktop_triggers(&self.opts.root);
        if !failed.is_empty() {
            bail!(
                "bundle {} installed with {} failure(s): {}",
                bundle.name,
                failed.len(),
                failed.join(", ")
            );
        }
        println!(
            "==> Installed bundle {} ({} installed, {} skipped)",
            bundle.name, installed, skipped
        );
        Ok(())
    }

    fn install_file(&mut self, path: &Path) -> Result<()> {
        let work = archive::temp_work_dir(&self.opts.root)?;
        let package = read_red(path, &work)?;
        package.verify()?;
        let manifest = package.manifest.clone();
        manifest.validate()?;

        if self.stack.iter().any(|name| name == &manifest.name) {
            bail!("dependency cycle detected at package {}", manifest.name);
        }

        let existing = self.db.packages.get(&manifest.name).cloned();
        let upgrading = existing.is_some();
        if let Some(old) = &existing {
            if old.manifest.full_version() == manifest.full_version() && !self.opts.force {
                bail!(
                    "{} {} is already installed (use --force to reinstall)",
                    manifest.name,
                    manifest.full_version()
                );
            }
            if !self.opts.force && !self.stack.is_empty() {
                // Dependencies are never force-downgraded silently.
                println!(
                    "==> Upgrading dependency {} {} -> {}",
                    manifest.name,
                    old.manifest.full_version(),
                    manifest.full_version()
                );
            }
        }

        self.resolve_dependencies(&manifest)?;
        self.check_conflicts(&manifest)?;

        let hooks = package.hook_scripts()?;
        if self.opts.hooks_enabled() {
            run_phase_hook(&self.opts.root, &hooks, "pre_install", &manifest, &work)?;
        }

        println!(
            "==> Installing {} {} ({})",
            manifest.name,
            manifest.full_version(),
            manifest.arch
        );

        let compression = package.compression()?;
        let tar_file = archive::decompress_to_temp(package.payload.path(), compression, &work)?;
        let stage = tempfile::Builder::new()
            .prefix(".red-stage-")
            .tempdir_in(&self.opts.root)
            .context("cannot create staging directory (are you root?)")?;
        let plan = extract_stage(tar_file.path(), stage.path())?;
        if plan.is_empty() {
            bail!("package payload contains no files");
        }

        let mut created = CreatedPaths::default();
        let result = (|| -> Result<()> {
            commit_stage(&self.opts.root, stage.path(), &plan, &self.db, &manifest.name, self.opts.force, &mut created)?;
            if let Some(old) = &existing {
                remove_stale_files(&self.opts.root, old, &plan, &self.db, &manifest.name)?;
            }
            let stored_hooks = if self.opts.hooks_enabled() {
                self.db.store_hooks(&manifest.name, &hooks)?
            } else {
                self.db.remove_hook_dir(&manifest.name);
                BTreeMap::new()
            };
            if self.opts.hooks_enabled() {
                if let Err(err) = run_phase_hook(&self.opts.root, &hooks, "post_install", &manifest, &work) {
                    self.db.remove_hook_dir(&manifest.name);
                    return Err(err);
                }
            }
            let record = build_record(
                manifest.clone(),
                plan.iter()
                    .filter(|entry| entry.kind != "dir")
                    .map(|entry| FileEntry {
                        path: entry.rel_string.clone(),
                        kind: entry.kind.to_string(),
                        size: entry.size,
                    })
                    .collect(),
                created.dirs.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
                stored_hooks,
            );
            self.db.save_record(&record)?;
            self.db.packages.insert(manifest.name.clone(), record);
            Ok(())
        })();

        if let Err(err) = result {
            rollback(&self.opts.root, &created);
            self.db.remove_hook_dir(&manifest.name);
            return Err(err);
        }

        let action = if upgrading { "Upgraded" } else { "Installed" };
        println!(
            "==> {action} {} {} ({} files, {} dirs, {})",
            manifest.name,
            manifest.full_version(),
            plan.iter().filter(|e| e.kind != "dir").count(),
            created.dirs.len(),
            human_size(plan.iter().map(|e| e.size).sum())
        );
        println!(
            "==> Run 'red -files {}' to list installed files",
            manifest.name
        );
        run_desktop_triggers(&self.opts.root);
        Ok(())
    }

    fn resolve_dependencies(&mut self, manifest: &Manifest) -> Result<()> {
        for dep in &manifest.depends {
            if self.db.is_satisfied(dep) {
                continue;
            }
            let constraint = Constraint::parse(dep);
            if constraint.name == manifest.name {
                continue;
            }
            match find_repo_for_dep(&self.opts.root, dep)? {
                Some(entry) => {
                    println!(
                        "==> Resolving dependency {} with {}",
                        dep,
                        entry.path.display()
                    );
                    self.stack.push(manifest.name.clone());
                    let result = self.install_file(&entry.path);
                    self.stack.pop();
                    result?;
                }
                None => {
                    if self.opts.nodeps {
                        eprintln!("red: warning: missing dependency {dep} (ignored: --nodeps)");
                    } else {
                        bail!(
                            "missing dependency: {dep} (add a repository with 'red -repo add <dir>' or use --nodeps)"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn check_conflicts(&self, manifest: &Manifest) -> Result<()> {
        for conflict in &manifest.conflicts {
            let constraint = Constraint::parse(conflict);
            for (name, record) in &self.db.packages {
                if name == &manifest.name {
                    continue;
                }
                if crate::db::record_satisfies(record, &constraint) {
                    if self.opts.force {
                        eprintln!(
                            "red: warning: {} conflicts with installed {} (continuing: --force)",
                            manifest.name, name
                        );
                    } else {
                        bail!("{} conflicts with installed package {}", manifest.name, name);
                    }
                }
            }
        }

        let owners = self.db.owners();
        for file in &manifest.files {
            if file.ends_with('/') {
                continue;
            }
            if let Some(owners) = owners.get(file) {
                if owners.iter().any(|owner| owner != &manifest.name) {
                    if self.opts.force {
                        eprintln!(
                            "red: warning: {} overwrites {} owned by {} (continuing: --force)",
                            manifest.name, file, owners.join(", ")
                        );
                    } else {
                        bail!(
                            "file conflict: {} is owned by {} (use --force to overwrite)",
                            file,
                            owners.join(", ")
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct StageEntry {
    rel: PathBuf,
    rel_string: String,
    kind: &'static str,
    size: u64,
    depth: usize,
}

fn extract_stage(tar_path: &Path, stage: &Path) -> Result<Vec<StageEntry>> {
    let file = File::open(tar_path)?;
    let mut archive = Archive::new(BufReader::new(file));
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    let mut entries: BTreeMap<PathBuf, StageEntry> = BTreeMap::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Some(rel) = sanitize_relative(&path)? else {
            continue;
        };
        let rel_string = rel.to_string_lossy().into_owned();
        if is_arch_metadata(&rel_string) {
            continue;
        }
        let entry_type = entry.header().entry_type();
        let kind = if entry_type.is_dir() {
            "dir"
        } else if entry_type.is_symlink() {
            "symlink"
        } else if entry_type.is_hard_link() {
            "hardlink"
        } else if entry_type.is_file() {
            "file"
        } else {
            continue;
        };
        let size = entry.header().size().unwrap_or(0);
        entry
            .unpack_in(stage)
            .with_context(|| format!("cannot extract {}", rel_string))?;
        let depth = rel.components().count();
        entries.insert(
            rel.clone(),
            StageEntry {
                rel,
                rel_string,
                kind,
                size,
                depth,
            },
        );
    }

    let mut list: Vec<StageEntry> = entries.into_values().collect();
    list.sort_by(|a, b| a.depth.cmp(&b.depth).then(a.rel_string.cmp(&b.rel_string)));
    Ok(list)
}

#[derive(Default)]
struct CreatedPaths {
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
}

fn commit_stage(
    root: &Path,
    stage: &Path,
    plan: &[StageEntry],
    db: &Database,
    package: &str,
    force: bool,
    created: &mut CreatedPaths,
) -> Result<()> {
    for entry in plan.iter().filter(|e| e.kind == "dir") {
        let dest = root.join(&entry.rel);
        match fs::symlink_metadata(&dest) {
            Ok(meta) => {
                if !meta.file_type().is_dir() {
                    bail!(
                        "cannot install directory {}: a file with that name already exists",
                        entry.rel_string
                    );
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(&dest)
                    .with_context(|| format!("cannot create directory {}", dest.display()))?;
                if let Ok(meta) = fs::metadata(stage.join(&entry.rel)) {
                    let mode = meta.permissions().mode();
                    let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(mode));
                }
                created.dirs.push(entry.rel.clone());
            }
            Err(err) => return Err(err.into()),
        }
    }

    for entry in plan.iter().filter(|e| e.kind != "dir") {
        let dest = root.join(&entry.rel);
        create_parents(root, &entry.rel, created)?;
        let owner = db.owner_of(&entry.rel_string);
        if let Some(owner) = owner {
            if owner != package && !force {
                bail!(
                    "file conflict: {} is owned by {} (use --force to overwrite)",
                    entry.rel_string,
                    owner
                );
            }
        }
        match fs::symlink_metadata(&dest) {
            Ok(meta) => {
                if meta.file_type().is_dir() {
                    bail!(
                        "cannot replace directory {} with a file",
                        entry.rel_string
                    );
                }
                fs::remove_file(&dest)
                    .with_context(|| format!("cannot replace {}", dest.display()))?;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        fs::rename(stage.join(&entry.rel), &dest)
            .with_context(|| format!("cannot install {}", dest.display()))?;
        created.files.push(entry.rel.clone());
    }
    Ok(())
}

fn create_parents(root: &Path, rel: &Path, created: &mut CreatedPaths) -> Result<()> {
    let Some(parent) = rel.parent() else {
        return Ok(());
    };
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        let dest = root.join(&current);
        match fs::symlink_metadata(&dest) {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&dest)
                    .with_context(|| format!("cannot create directory {}", dest.display()))?;
                created.dirs.push(current.clone());
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

fn rollback(root: &Path, created: &CreatedPaths) {
    for path in created.files.iter().rev() {
        let _ = remove_file_if_exists(&root.join(path));
    }
    let mut dirs: Vec<&PathBuf> = created.dirs.iter().collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for dir in dirs {
        let _ = fs::remove_dir(root.join(dir));
    }
}

fn remove_stale_files(
    root: &Path,
    old: &InstalledRecord,
    new_plan: &[StageEntry],
    db: &Database,
    package: &str,
) -> Result<()> {
    let new_files: BTreeSet<&str> = new_plan.iter().map(|e| e.rel_string.as_str()).collect();
    for file in &old.files {
        if new_files.contains(file.path.as_str()) {
            continue;
        }
        if let Some(owner) = db.owner_of(&file.path) {
            if owner != package {
                continue;
            }
        }
        let path = root.join(&file.path);
        if let Err(err) = remove_file_if_exists(&path) {
            eprintln!("red: warning: {err}");
            continue;
        }
        prune_empty_parents(&path, root);
    }
    Ok(())
}

fn run_phase_hook(
    root: &Path,
    hooks: &BTreeMap<String, Vec<u8>>,
    phase: &str,
    manifest: &Manifest,
    work: &Path,
) -> Result<()> {
    if let Some(script) = hooks.get("arch_install") {
        let temp = write_temp_script(work, "arch-install.sh", script)?;
        return run_arch_hook(root, temp.path(), phase, manifest);
    }
    if let Some(script) = hooks.get(phase) {
        let temp = write_temp_script(work, &format!("{phase}.sh"), script)?;
        let action = match phase {
            "pre_install" => "install",
            "post_install" => "configure",
            "pre_remove" | "post_remove" => "remove",
            _ => "unknown",
        };
        return run_script_hook(root, temp.path(), phase, action, manifest);
    }
    Ok(())
}

fn write_temp_script(work: &Path, name: &str, data: &[u8]) -> Result<NamedTempFile> {
    let mut file = tempfile::Builder::new()
        .prefix("red-hook-")
        .suffix(&format!("-{name}"))
        .tempfile_in(work)?;
    use std::io::Write;
    file.write_all(data)?;
    file.as_file_mut().flush()?;
    let mut perms = fs::metadata(file.path())?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(file.path(), perms)?;
    Ok(file)
}

fn hook_env<'a>(command: &'a mut Command, root: &Path, manifest: &Manifest) -> &'a mut Command {
    command
        .env("RED_ROOT", root.display().to_string())
        .env("RED_DB", Database::db_dir(root).display().to_string())
        .env("RED_PACKAGE", &manifest.name)
        .env("RED_NAME", &manifest.name)
        .env("RED_VERSION", manifest.full_version())
        .env("RED_ARCH", &manifest.arch)
        .env("RED_ORIGIN", &manifest.origin)
}

fn run_script_hook(
    root: &Path,
    script: &Path,
    phase: &str,
    action: &str,
    manifest: &Manifest,
) -> Result<()> {
    let mut command = Command::new("bash");
    command.arg(script).arg(action);
    hook_env(&mut command, root, manifest);
    command.current_dir(root);
    let status = command
        .status()
        .with_context(|| format!("cannot run {phase} hook"))?;
    if !status.success() {
        bail!("{phase} hook failed with status {status}");
    }
    Ok(())
}

fn run_arch_hook(root: &Path, script: &Path, phase: &str, manifest: &Manifest) -> Result<()> {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg("set -e; source \"$1\"; if declare -F \"$2\" >/dev/null 2>&1; then \"$2\"; fi")
        .arg("red-hook")
        .arg(script)
        .arg(phase);
    hook_env(&mut command, root, manifest);
    command.current_dir(root);
    let status = command
        .status()
        .with_context(|| format!("cannot run {phase} hook"))?;
    if !status.success() {
        bail!("{phase} hook failed with status {status}");
    }
    Ok(())
}

pub fn remove(root: &Path, name: &str, force: bool, run_hooks: bool) -> Result<()> {
    let db = Database::load(root)?;
    let record = match db.packages.get(name) {
        Some(record) => record.clone(),
        None => bail!("package not installed: {name}"),
    };

    let dependents = db.reverse_dependents(name);
    if !dependents.is_empty() {
        if force {
            eprintln!(
                "red: warning: removing {} breaks dependencies of {} (continuing: --force)",
                name,
                dependents.join(", ")
            );
        } else {
            bail!(
                "cannot remove {}: required by {} (use --force)",
                name,
                dependents.join(", ")
            );
        }
    }

    if run_hooks {
        run_stored_hook(root, &db, &record, "pre_remove")?;
    }

    let mut files: Vec<&FileEntry> = record.files.iter().collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.path.matches('/').count()));
    for file in files {
        let path = root.join(&file.path);
        if let Err(err) = remove_file_if_exists(&path) {
            eprintln!("red: warning: {err}");
        }
        prune_empty_parents(&path, root);
    }

    let mut dirs: Vec<&String> = record.directories.iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    for dir in dirs {
        let path = root.join(dir);
        match fs::remove_dir(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }

    if run_hooks {
        run_stored_hook(root, &db, &record, "post_remove")?;
    }

    db.delete_record(name)?;
    db.remove_hook_dir(name);
    run_desktop_triggers(root);
    println!("==> Removed {} {}", name, record.manifest.full_version());
    Ok(())
}

/// Refresh desktop, icon and MIME caches so newly installed applications
/// appear in the launcher exactly like natively installed packages.
pub fn run_desktop_triggers(root: &Path) {
    let applications = root.join("usr/share/applications");
    if applications.is_dir() {
        run_tool("update-desktop-database", &[applications.as_os_str()]);
    }
    let icons = root.join("usr/share/icons/hicolor");
    if icons.is_dir() {
        run_tool(
            "gtk-update-icon-cache",
            &[OsStr::new("-f"), OsStr::new("-t"), icons.as_os_str()],
        );
    }
    let mime = root.join("usr/share/mime");
    if mime.is_dir() {
        run_tool("update-mime-database", &[mime.as_os_str()]);
    }
}

fn run_tool(program: &str, args: &[&OsStr]) {
    if let Ok(status) = Command::new(program).args(args).status() {
        if !status.success() {
            eprintln!("red: warning: {program} exited with {status}");
        }
    }
}

fn run_stored_hook(
    root: &Path,
    db: &Database,
    record: &InstalledRecord,
    phase: &str,
) -> Result<()> {
    if let Some(rel) = record.hooks.get("arch_install") {
        let script = db.hook_path(rel);
        if script.exists() {
            return run_arch_hook(root, &script, phase, &record.manifest);
        }
    }
    if let Some(rel) = record.hooks.get(phase) {
        let script = db.hook_path(rel);
        if script.exists() {
            let action = match phase {
                "pre_remove" | "post_remove" => "remove",
                _ => "unknown",
            };
            return run_script_hook(root, &script, phase, action, &record.manifest);
        }
    }
    Ok(())
}

pub fn list_installed(root: &Path) -> Result<()> {
    let db = Database::load(root)?;
    if db.packages.is_empty() {
        println!("No packages installed.");
        return Ok(());
    }
    let mut records: Vec<&InstalledRecord> = db.packages.values().collect();
    records.sort_by_key(|r| r.manifest.name.clone());
    let name_width = records
        .iter()
        .map(|r| r.manifest.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    for record in records {
        println!(
            "{:<width$}  {:<12}  {:<12}  {}",
            record.manifest.name,
            record.manifest.full_version(),
            record.manifest.arch,
            record.manifest.description,
            width = name_width
        );
    }
    println!("\n{} package(s) installed.", db.packages.len());
    Ok(())
}

pub fn print_info(root: &Path, target: &str) -> Result<()> {
    let path = PathBuf::from(target);
    if path.is_file() {
        if is_bundle_path(&path) {
            let bundle = crate::bundle::read_bundle(&path)?;
            print_field("Name", &bundle.name);
            print_field("Type", "ARED bundle");
            print_field("Packages", &bundle.packages.len().to_string());
            print_field("Total Size", &human_size(bundle.total_size()));
            for entry in &bundle.packages {
                println!(
                    "  {}-{}-{}  {}  ({})",
                    entry.name,
                    entry.version,
                    entry.arch,
                    entry.description,
                    entry.path
                );
            }
            return Ok(());
        }
        let manifest = read_manifest(&path)?;
        print_manifest(&manifest, None);
        return Ok(());
    }
    let db = Database::load(root)?;
    if let Some(record) = db.packages.get(target) {
        print_manifest(&record.manifest, Some(record));
        return Ok(());
    }
    if let Some(entry) = find_repo_by_name(root, target)? {
        print_manifest(&entry.manifest, None);
        println!("{:<18}: {}", "Repository file", entry.path.display());
        return Ok(());
    }
    bail!("package not found: {target}")
}

fn print_manifest(manifest: &Manifest, record: Option<&InstalledRecord>) {
    print_field("Name", &manifest.name);
    print_field("Version", &manifest.full_version());
    print_field("Architecture", &manifest.arch);
    print_field("Description", &manifest.description);
    print_field("URL", &manifest.url);
    print_field("Licenses", &manifest.license.join(" "));
    print_field("Maintainer", &manifest.maintainer);
    print_field("Depends On", &manifest.depends.join("  "));
    print_field("Optional Deps", &manifest.opt_depends.join("  "));
    print_field("Provides", &manifest.provides.join("  "));
    print_field("Conflicts With", &manifest.conflicts.join("  "));
    print_field("Replaces", &manifest.replaces.join("  "));
    if !manifest.foreign_depends.is_empty() {
        print_field("Foreign Deps", &manifest.foreign_depends.join("  "));
    }
    print_field("Origin", &manifest.origin);
    print_field("Installed Size", &human_size(manifest.installed_size));
    print_field("Package Size", &human_size(manifest.payload_size));
    print_field("SHA256", &manifest.sha256);
    print_field(
        "Built At",
        &if manifest.built_at > 0 {
            format!("unix:{}", manifest.built_at)
        } else {
            String::new()
        },
    );
    if let Some(record) = record {
        print_field("Installed", &format!("yes ({} files)", record.files.len()));
        print_field("Disk Usage", &human_size(record.disk_usage()));
        print_field("Installed At", &format!("unix:{}", record.installed_at));
    }
}

pub fn print_files(root: &Path, target: &str) -> Result<()> {
    let path = PathBuf::from(target);
    if path.is_file() {
        let manifest = read_manifest(&path)?;
        for file in &manifest.files {
            println!("{file}");
        }
        return Ok(());
    }
    let db = Database::load(root)?;
    if let Some(record) = db.packages.get(target) {
        for dir in &record.directories {
            println!("{dir}/");
        }
        for file in &record.files {
            println!("{}", file.path);
        }
        return Ok(());
    }
    if let Some(entry) = find_repo_by_name(root, target)? {
        for file in &entry.manifest.files {
            println!("{file}");
        }
        return Ok(());
    }
    bail!("package not found: {target}")
}

pub fn verify(path: &Path) -> Result<()> {
    let work = std::env::temp_dir().join("red-verify");
    let package = read_red(path, &work)?;
    package.verify()?;
    println!(
        "==> OK: {} {} ({})",
        package.manifest.name,
        package.manifest.full_version(),
        human_size(package.manifest.payload_size)
    );
    Ok(())
}

pub fn search(root: &Path, term: &str) -> Result<()> {
    let entries = scan_repos(root)?;
    let needle = term.to_ascii_lowercase();
    let matches: Vec<_> = entries
        .into_iter()
        .filter(|entry| {
            entry.manifest.name.to_ascii_lowercase().contains(&needle)
                || entry.manifest.description.to_ascii_lowercase().contains(&needle)
        })
        .collect();
    if matches.is_empty() {
        println!("No packages found for '{term}'.");
        return Ok(());
    }
    for entry in matches {
        println!(
            "{}-{}-{}  {}",
            entry.manifest.name,
            entry.manifest.full_version(),
            entry.manifest.arch,
            entry.manifest.description
        );
        println!("    {}", entry.path.display());
    }
    Ok(())
}

pub fn list_repos(root: &Path) -> Result<()> {
    let repos = crate::db::load_repos(root)?;
    if repos.is_empty() {
        println!("No repositories configured.");
        return Ok(());
    }
    for repo in repos {
        println!("{}", repo.display());
    }
    Ok(())
}
