use crate::util::human_size;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// QML front end, written next to this binary at build time and copied into a
/// per-run runtime directory before quickshell is started.
const QML: &str = include_str!("../gui/shell.qml");

#[derive(Serialize)]
struct GuiAction {
    id: &'static str,
    label: &'static str,
    icon: &'static str,
}

#[derive(Serialize)]
struct GuiState {
    file: String,
    name: String,
    kind: &'static str,
    badge: String,
    size: String,
    icon: &'static str,
    summary: String,
    detail: String,
    actions: Vec<GuiAction>,
    arch: &'static str,
    root: bool,
    quickshell: String,
    theme: GuiTheme,
}

/// Colour tokens handed to the QML front end. Filled from the running desktop's
/// Material theme so the window matches the rest of the environment.
#[derive(Serialize)]
struct GuiTheme {
    mode: String,
    surface: String,
    surface_low: String,
    surface_high: String,
    surface_highest: String,
    foreground: String,
    foreground_soft: String,
    muted: String,
    outline: String,
    outline_variant: String,
    primary: String,
    on_primary: String,
    danger: String,
}

impl GuiTheme {
    fn dark() -> Self {
        Self {
            mode: "dark".to_string(),
            surface: "#101418".to_string(),
            surface_low: "#191c20".to_string(),
            surface_high: "#1d2024".to_string(),
            surface_highest: "#272a2f".to_string(),
            foreground: "#e0e2e8".to_string(),
            foreground_soft: "#c2c7cf".to_string(),
            muted: "#8c9199".to_string(),
            outline: "#8c9199".to_string(),
            outline_variant: "#42474e".to_string(),
            primary: "#42a5f5".to_string(),
            on_primary: "#000000".to_string(),
            danger: "#f2b8b5".to_string(),
        }
    }

    /// Read DankMaterialShell's generated palette; fall back to the built-in dark
    /// palette when the file is missing or malformed.
    fn from_system() -> Self {
        let Some(path) = theme_file() else {
            return Self::dark();
        };
        let Ok(raw) = fs::read_to_string(&path) else {
            return Self::dark();
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Self::dark();
        };
        let mode = json
            .get("mode")
            .and_then(|mode| mode.as_str())
            .unwrap_or("dark");
        let Some(colors) = json.get("colors").and_then(|colors| colors.get(mode)) else {
            return Self::dark();
        };

        let base = Self::dark();
        let mut theme = Self {
            mode: mode.to_string(),
            ..base
        };
        let pick = |key: &str, slot: &mut String| {
            if let Some(value) = colors.get(key).and_then(|value| value.as_str()) {
                *slot = value.to_string();
            }
        };
        pick("surface", &mut theme.surface);
        pick("surface_container_low", &mut theme.surface_low);
        pick("surface_container_high", &mut theme.surface_high);
        pick("surface_container_highest", &mut theme.surface_highest);
        pick("on_surface", &mut theme.foreground);
        pick("on_surface_variant", &mut theme.foreground_soft);
        pick("outline", &mut theme.outline);
        pick("outline_variant", &mut theme.outline_variant);
        pick("primary", &mut theme.primary);
        pick("on_primary", &mut theme.on_primary);
        pick("error", &mut theme.danger);
        if let Some(muted) = colors.get("outline").and_then(|value| value.as_str()) {
            theme.muted = muted.to_string();
        }
        theme
    }
}

fn theme_file() -> Option<PathBuf> {
    if let Some(explicit) = env::var_os("RED_THEME_FILE") {
        return Some(PathBuf::from(explicit));
    }
    let cache = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(cache.join("DankMaterialShell").join("dms-colors.json"))
}

#[derive(PartialEq, Eq)]
enum Kind {
    Red,
    Bundle,
    Archive,
    Foreign,
    AurDir,
    Directory,
    Missing,
    Unsupported,
}

pub fn run(startup: Option<&Path>) -> Result<()> {
    let state = state_for(startup);
    let quickshell = find_quickshell().context(
        "找不到 quickshell。RED 的图形界面基于 quickshell（DankMaterialShell），请先安装：\n  sudo pacman -S quickshell-git",
    )?;

    let dir = runtime_dir().join(format!("red-gui-{}", std::process::id()));
    fs::create_dir_all(&dir).with_context(|| format!("无法创建 {}", dir.display()))?;
    fs::write(dir.join("shell.qml"), QML)
        .with_context(|| format!("无法写入 {}", dir.join("shell.qml").display()))?;

    let json = serde_json::to_string(&state)?;
    let mut cmd = Command::new(&quickshell);
    cmd.arg("-p")
        .arg(&dir)
        .env("RED_GUI_STATE", json)
        .env("RED_GUI_QUICKSHELL", &quickshell);
    if let Ok(exe) = env::current_exe() {
        cmd.env("RED_GUI_BIN", exe);
    }

    let status = cmd.status();
    let _ = fs::remove_dir_all(&dir);

    let status = status.context("无法启动 quickshell")?;
    if !status.success() {
        bail!("quickshell 退出码 {status}");
    }
    Ok(())
}

fn state_for(file: Option<&Path>) -> GuiState {
    let arch = env::consts::ARCH;
    let root = unsafe { libc::geteuid() } == 0;
    let theme = GuiTheme::from_system();
    let quickshell = find_quickshell()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();

    let Some(path) = file else {
        return GuiState {
            file: String::new(),
            name: "未选择文件".to_string(),
            kind: "none",
            badge: String::new(),
            size: String::new(),
            icon: "🫥",
            summary: "右键 tar.gz 或 .red 文件，选择“使用 RED 打开”".to_string(),
            detail: "也可以在终端运行：red -gui <文件>".to_string(),
            actions: Vec::new(),
            arch,
            root,
            quickshell,
            theme,
        };
    };

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let kind = classify(path);

    // Bundle contents / bundle-able directories enrich summary, detail and actions.
    let bundle_info = if kind == Kind::Bundle {
        crate::bundle::read_bundle(path).ok()
    } else {
        None
    };
    let red_count = if kind == Kind::Directory {
        crate::bundle::find_red_files(path)
            .map(|files| files.len())
            .unwrap_or(0)
    } else {
        0
    };

    let (kind_name, icon, badge, summary, actions) = match kind {
        Kind::Red => (
            "red",
            "😋",
            "RED".to_string(),
            "RED 软件包，可直接安装".to_string(),
            vec![GuiAction {
                id: "install",
                label: "安装 RED 包",
                icon: "⬇️",
            }],
        ),
        Kind::Bundle => {
            let count = bundle_info
                .as_ref()
                .map(|bundle| bundle.packages.len())
                .unwrap_or(0);
            (
                "ared",
                "📚",
                "ARED".to_string(),
                format!("RED 合集包，内含 {count} 个应用，可一键全部安装"),
                vec![GuiAction {
                    id: "install",
                    label: "一键安装全部",
                    icon: "⬇️",
                }],
            )
        }
        Kind::Archive => (
            "archive",
            "🤔",
            archive_badge(path),
            "归档包，可构建为自包含的 .red".to_string(),
            vec![
                GuiAction {
                    id: "build",
                    label: "构建 RED 包",
                    icon: "📦",
                },
                GuiAction {
                    id: "system",
                    label: "构建适用当前系统的包",
                    icon: "🖥️",
                },
            ],
        ),
        Kind::Foreign => (
            "foreign",
            "🤨",
            archive_badge(path),
            "外来包，转换为 .red（需要对应包管理器）".to_string(),
            vec![GuiAction {
                id: "rebuild",
                label: "转换为 RED",
                icon: "🔁",
            }],
        ),
        Kind::AurDir => (
            "aur",
            "🧩",
            "PKGBUILD".to_string(),
            "AUR 目录，调用 makepkg 后转换为 .red".to_string(),
            vec![GuiAction {
                id: "rebuild",
                label: "构建并转换",
                icon: "🔁",
            }],
        ),
        Kind::Directory => {
            let mut actions = vec![
                GuiAction {
                    id: "build",
                    label: "构建 RED 包",
                    icon: "📦",
                },
                GuiAction {
                    id: "system",
                    label: "构建适用当前系统的包",
                    icon: "�️",
                },
            ];
            if red_count > 0 {
                actions.push(GuiAction {
                    id: "bundle",
                    label: "构建 ARED 合集包",
                    icon: "�",
                });
            }
            let summary = if red_count > 0 {
                format!("目录，内含 {red_count} 个 .red 包，可打包为合集")
            } else {
                "目录，打包为 .red".to_string()
            };
            ("directory", "📁", "DIR".to_string(), summary, actions)
        }
        Kind::Missing => (
            "missing",
            "😵",
            String::new(),
            "文件不存在或路径无法访问".to_string(),
            Vec::new(),
        ),
        Kind::Unsupported => (
            "unsupported",
            "😐",
            String::new(),
            "支持 .red、.ared、.tar.*、.deb、.rpm、.pkg.tar.* 与目录".to_string(),
            Vec::new(),
        ),
    };

    let size = fs::metadata(path)
        .map(|meta| {
            if meta.is_dir() {
                "目录".to_string()
            } else {
                human_size(meta.len())
            }
        })
        .unwrap_or_else(|_| "未知".to_string());

    let mut detail = format!(
        "路径 {}\n类型 {}\n大小 {}\n架构 {}{}",
        path.display(),
        kind_name,
        size,
        arch,
        if root { "\n权限 root" } else { "" }
    );
    if let Some(bundle) = &bundle_info {
        detail.push_str("\n\n包含的包：");
        for entry in &bundle.packages {
            detail.push_str(&format!(
                "\n  {} {} ({})  {}",
                entry.name, entry.version, entry.arch, entry.path
            ));
        }
    }

    GuiState {
        file: path.to_string_lossy().into_owned(),
        name,
        kind: kind_name,
        badge,
        size,
        icon,
        summary,
        detail,
        actions,
        arch,
        root,
        quickshell,
        theme,
    }
}

fn classify(path: &Path) -> Kind {
    if !path.exists() {
        return Kind::Missing;
    }
    if path.is_dir() {
        return if path.join("PKGBUILD").exists() {
            Kind::AurDir
        } else {
            Kind::Directory
        };
    }
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".ared") {
        return Kind::Bundle;
    }
    if lower.ends_with(".red") {
        return Kind::Red;
    }
    if lower.ends_with(".deb") || lower.ends_with(".rpm") || lower.contains(".pkg.tar") {
        return Kind::Foreign;
    }
    if lower.ends_with(".tar")
        || lower.contains(".tar.")
        || lower.ends_with(".tgz")
        || lower.ends_with(".txz")
        || lower.ends_with(".tzst")
    {
        return Kind::Archive;
    }
    Kind::Unsupported
}

fn archive_badge(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name.ends_with(".deb") {
        return "DEB".to_string();
    }
    if name.ends_with(".rpm") {
        return "RPM".to_string();
    }
    if name.contains(".pkg.tar.") {
        return "ARCH".to_string();
    }
    let ext = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    if name.contains(".tar.") {
        return format!("TAR.{}", ext.to_ascii_uppercase());
    }
    if ext == "tgz" {
        return "TGZ".to_string();
    }
    ext.to_ascii_uppercase()
}

fn find_quickshell() -> Option<PathBuf> {
    if let Some(explicit) = env::var_os("RED_QUICKSHELL") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    for name in ["quickshell", "qs"] {
        if let Some(path) = find_in_path(name) {
            return Some(path);
        }
    }
    None
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for dir in ["/usr/bin", "/usr/local/bin", "/bin"] {
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn runtime_dir() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| env::temp_dir())
}
