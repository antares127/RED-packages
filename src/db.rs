use crate::archive::read_manifest;
use crate::manifest::Manifest;
use crate::util::{now_unix, write_executable};
use crate::version::{version_cmp, Constraint};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path relative to the installation root.
    pub path: String,
    /// file | symlink | hardlink
    pub kind: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledRecord {
    pub manifest: Manifest,
    #[serde(default)]
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub directories: Vec<String>,
    #[serde(default)]
    pub installed_at: u64,
    /// phase -> path relative to <root>/var/lib/red
    #[serde(default)]
    pub hooks: BTreeMap<String, String>,
}

impl InstalledRecord {
    pub fn disk_usage(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RepoConfig {
    #[serde(default)]
    pub repos: Vec<String>,
}

pub struct Database {
    pub root: PathBuf,
    pub packages: BTreeMap<String, InstalledRecord>,
}

impl Database {
    pub fn load(root: &Path) -> Result<Self> {
        let installed_dir = Self::db_dir(root).join("installed");
        fs::create_dir_all(&installed_dir)
            .with_context(|| format!("cannot create {}", installed_dir.display()))?;
        let mut packages = BTreeMap::new();
        for entry in fs::read_dir(&installed_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            let record: InstalledRecord = serde_json::from_str(&text)
                .with_context(|| format!("corrupt database entry {}", path.display()))?;
            packages.insert(record.manifest.name.clone(), record);
        }
        Ok(Database {
            root: root.to_path_buf(),
            packages,
        })
    }

    pub fn db_dir(root: &Path) -> PathBuf {
        root.join("var/lib/red")
    }

    pub fn hooks_dir(&self, name: &str) -> PathBuf {
        Self::db_dir(&self.root).join("hooks").join(name)
    }

    pub fn save_record(&self, record: &InstalledRecord) -> Result<()> {
        let dir = Self::db_dir(&self.root).join("installed");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", record.manifest.name));
        let tmp = tempfile::Builder::new()
            .prefix(".red-db-")
            .tempfile_in(&dir)?;
        fs::write(tmp.path(), serde_json::to_string_pretty(record)?)?;
        tmp.persist(&path)
            .map_err(|err| anyhow::anyhow!("cannot save {}: {}", path.display(), err.error))?;
        Ok(())
    }

    pub fn delete_record(&self, name: &str) -> Result<()> {
        let path = Self::db_dir(&self.root)
            .join("installed")
            .join(format!("{name}.json"));
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| format!("cannot remove {}", path.display())),
        }
    }

    pub fn store_hooks(
        &self,
        name: &str,
        hooks: &BTreeMap<String, Vec<u8>>,
    ) -> Result<BTreeMap<String, String>> {
        let dir = self.hooks_dir(name);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        let mut stored = BTreeMap::new();
        for (phase, data) in hooks {
            let file = dir.join(format!("{phase}.sh"));
            write_executable(&file, data)?;
            stored.insert(
                phase.clone(),
                format!("hooks/{name}/{phase}.sh"),
            );
        }
        Ok(stored)
    }

    pub fn hook_path(&self, rel: &str) -> PathBuf {
        Self::db_dir(&self.root).join(rel)
    }

    pub fn remove_hook_dir(&self, name: &str) {
        let dir = self.hooks_dir(name);
        if dir.exists() {
            let _ = fs::remove_dir_all(dir);
        }
    }

    pub fn owner_of(&self, path: &str) -> Option<&str> {
        for (name, record) in &self.packages {
            if record.files.iter().any(|f| f.path == path) {
                return Some(name);
            }
        }
        None
    }

    pub fn owners(&self) -> HashMap<String, Vec<String>> {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for (name, record) in &self.packages {
            for file in &record.files {
                map.entry(file.path.clone()).or_default().push(name.clone());
            }
        }
        map
    }

    pub fn provider(&self, constraint: &Constraint) -> Option<&InstalledRecord> {
        self.packages
            .values()
            .find(|record| record_satisfies(record, constraint))
    }

    pub fn is_satisfied(&self, dep: &str) -> bool {
        let constraint = Constraint::parse(dep);
        self.provider(&constraint).is_some()
    }

    /// Other installed packages that depend on `name`.
    pub fn reverse_dependents(&self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (pkg_name, record) in &self.packages {
            if pkg_name == name {
                continue;
            }
            for dep in &record.manifest.depends {
                let constraint = Constraint::parse(dep);
                if constraint.name != name {
                    continue;
                }
                // Would removing this package break the dependency?
                let satisfied_elsewhere = self.packages.values().any(|other| {
                    other.manifest.name != name && record_satisfies(other, &constraint)
                });
                if !satisfied_elsewhere {
                    out.push(pkg_name.clone());
                    break;
                }
            }
        }
        out
    }
}

pub fn record_satisfies(record: &InstalledRecord, constraint: &Constraint) -> bool {
    if record.manifest.name == constraint.name
        && constraint.matches_version(&record.manifest.full_version())
    {
        return true;
    }
    for provide in &record.manifest.provides {
        let parsed = Constraint::parse(provide);
        if parsed.name != constraint.name {
            continue;
        }
        if constraint.op == crate::version::Op::Any || constraint.version.is_empty() {
            return true;
        }
        if parsed.version.is_empty() {
            // Unversioned provides satisfy versioned dependencies.
            return true;
        }
        if constraint.matches_version(&parsed.version) {
            return true;
        }
    }
    false
}

pub fn manifest_satisfies(manifest: &Manifest, constraint: &Constraint) -> bool {
    if manifest.name == constraint.name && constraint.matches_version(&manifest.full_version()) {
        return true;
    }
    for provide in &manifest.provides {
        let parsed = Constraint::parse(provide);
        if parsed.name != constraint.name {
            continue;
        }
        if constraint.op == crate::version::Op::Any || constraint.version.is_empty() {
            return true;
        }
        if parsed.version.is_empty() || constraint.matches_version(&parsed.version) {
            return true;
        }
    }
    false
}

pub struct RepoEntry {
    pub path: PathBuf,
    pub manifest: Manifest,
}

pub fn repo_config_path(root: &Path) -> PathBuf {
    Database::db_dir(root).join("repos.json")
}

pub fn load_repos(root: &Path) -> Result<Vec<PathBuf>> {
    let path = repo_config_path(root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path)?;
    let config: RepoConfig = serde_json::from_str(&text)
        .with_context(|| format!("invalid repo config {}", path.display()))?;
    Ok(config.repos.into_iter().map(PathBuf::from).collect())
}

pub fn save_repos(root: &Path, repos: &[PathBuf]) -> Result<()> {
    let path = repo_config_path(root);
    fs::create_dir_all(path.parent().unwrap())?;
    let config = RepoConfig {
        repos: repos
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
    };
    fs::write(&path, serde_json::to_string_pretty(&config)?)?;
    Ok(())
}

pub fn scan_repos(root: &Path) -> Result<Vec<RepoEntry>> {
    let mut out = Vec::new();
    for repo in load_repos(root)? {
        if !repo.is_dir() {
            eprintln!("red: warning: repository {} does not exist", repo.display());
            continue;
        }
        for entry in fs::read_dir(&repo)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("red") {
                continue;
            }
            match read_manifest(&path) {
                Ok(manifest) => out.push(RepoEntry { path, manifest }),
                Err(err) => eprintln!(
                    "red: warning: skipping invalid package {}: {err}",
                    path.display()
                ),
            }
        }
    }
    Ok(out)
}

pub fn find_repo_by_name(root: &Path, name: &str) -> Result<Option<RepoEntry>> {
    let entries = scan_repos(root)?;
    let mut best: Option<RepoEntry> = None;
    for entry in entries {
        if entry.manifest.name != name {
            continue;
        }
        match &best {
            Some(current)
                if version_cmp(&current.manifest.full_version(), &entry.manifest.full_version())
                    != Ordering::Less => {}
            _ => best = Some(entry),
        }
    }
    Ok(best)
}

pub fn find_repo_for_dep(root: &Path, dep: &str) -> Result<Option<RepoEntry>> {
    let constraint = Constraint::parse(dep);
    let entries = scan_repos(root)?;
    let mut best: Option<RepoEntry> = None;
    for entry in entries {
        if !manifest_satisfies(&entry.manifest, &constraint) {
            continue;
        }
        match &best {
            Some(current)
                if version_cmp(&current.manifest.full_version(), &entry.manifest.full_version())
                    != Ordering::Less => {}
            _ => best = Some(entry),
        }
    }
    Ok(best)
}

pub fn add_repo(root: &Path, path: &Path) -> Result<()> {
    if !path.is_dir() {
        bail!("not a directory: {}", path.display());
    }
    let canonical = path
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", path.display()))?;
    let mut repos = load_repos(root)?;
    if repos.iter().any(|p| p == &canonical) {
        println!("Repository already registered: {}", canonical.display());
        return Ok(());
    }
    repos.push(canonical.clone());
    save_repos(root, &repos)?;
    println!("Added repository: {}", canonical.display());
    Ok(())
}

pub fn remove_repo(root: &Path, path: &Path) -> Result<()> {
    let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut repos = load_repos(root)?;
    let before = repos.len();
    repos.retain(|p| p != &target && p != path);
    if repos.len() == before {
        bail!("repository not registered: {}", path.display());
    }
    save_repos(root, &repos)?;
    println!("Removed repository: {}", target.display());
    Ok(())
}

pub fn build_record(
    manifest: Manifest,
    files: Vec<FileEntry>,
    directories: Vec<String>,
    hooks: BTreeMap<String, String>,
) -> InstalledRecord {
    InstalledRecord {
        manifest,
        files,
        directories,
        installed_at: now_unix(),
        hooks,
    }
}
