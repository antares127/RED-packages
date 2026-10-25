mod archive;
mod build;
mod bundle;
mod db;
mod gui;
mod install;
mod manifest;
mod util;
mod version;

use anyhow::{bail, Result};
use build::BuildOptions;
use install::InstallOptions;
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    if let Err(err) = run() {
        eprintln!("red: error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        return Ok(());
    }
    let command = args[0].trim_start_matches('-');
    let rest = &args[1..];
    match command {
        "h" | "help" => {
            match rest.first().map(|s| s.as_str()) {
                Some("build") => print_build_help(),
                Some("rebuild") => print_rebuild_help(),
                _ => print_usage(),
            }
            Ok(())
        }
        "v" | "version" => {
            println!("red {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "build" | "b" => cmd_build(rest),
        "rebuild" | "rb" => cmd_rebuild(rest),
        "bundle" => cmd_bundle(rest),
        "install" | "i" => cmd_install(rest),
        "remove" | "uninstall" | "r" => cmd_remove(rest),
        "list" | "l" => cmd_list(rest),
        "info" | "show" | "q" => cmd_info(rest),
        "files" | "f" => cmd_files(rest),
        "verify" => cmd_verify(rest),
        "search" | "s" => cmd_search(rest),
        "repo" => cmd_repo(rest),
        "gui" | "g" => cmd_gui(rest),
        other => bail!("unknown command '{other}' (run 'red -help')"),
    }
}

struct ParsedBuild {
    opts: BuildOptions,
    sources: Vec<String>,
}

fn cmd_build(args: &[String]) -> Result<()> {
    let Some(parsed) = parse_build_args(args, false)? else {
        return Ok(());
    };
    run_build_sources(&parsed, false)
}

fn cmd_rebuild(args: &[String]) -> Result<()> {
    let Some(parsed) = parse_build_args(args, true)? else {
        return Ok(());
    };
    run_build_sources(&parsed, true)
}

fn run_build_sources(parsed: &ParsedBuild, rebuild: bool) -> Result<()> {
    if parsed.sources.len() > 1 && parsed.opts.out.is_some() {
        bail!("--out can only be used when building a single package");
    }
    for source in &parsed.sources {
        let path = PathBuf::from(source);
        if rebuild {
            build::rebuild(path.as_path(), &parsed.opts)?;
        } else {
            build::build(path.as_path(), &parsed.opts)?;
        }
    }
    Ok(())
}

fn cmd_bundle(args: &[String]) -> Result<()> {
    let mut dir: Option<String> = None;
    let mut name: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" | "-n" => {
                name = Some(value(args, i, "--name")?);
                i += 2;
            }
            "--out" | "-o" => {
                out = Some(PathBuf::from(value(args, i, "--out")?));
                i += 2;
            }
            "--help" | "-h" => {
                println!(
                    "usage: red -bundle <dir> [--name NAME] [--out FILE.ared]\n\n\
                     Pack every .red package under <dir> (recursively) into a single\n\
                     .ared bundle, keeping the directory layout. Install the bundle\n\
                     with 'red -install <file.ared>' to install all packages at once."
                );
                return Ok(());
            }
            other if other.starts_with('-') => bail!("unknown bundle option '{other}'"),
            _ => {
                if dir.is_some() {
                    bail!("only one directory can be bundled at a time");
                }
                dir = Some(args[i].clone());
                i += 1;
            }
        }
    }
    let dir = dir.ok_or_else(|| anyhow::anyhow!("usage: red -bundle <dir> [--out FILE.ared]"))?;
    bundle::build_bundle(PathBuf::from(&dir).as_path(), name, out)?;
    Ok(())
}

fn parse_build_args(args: &[String], rebuild: bool) -> Result<Option<ParsedBuild>> {
    let mut opts = BuildOptions::default();
    let mut sources: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--name" | "-n" => {
                opts.name = Some(value(args, i, arg)?);
                i += 2;
            }
            "--version" | "-V" => {
                opts.version = Some(value(args, i, arg)?);
                i += 2;
            }
            "--release" => {
                let raw = value(args, i, arg)?;
                opts.release = Some(
                    raw.parse()
                        .map_err(|_| anyhow::anyhow!("invalid release number: {raw}"))?,
                );
                i += 2;
            }
            "--arch" | "-a" => {
                opts.arch = Some(value(args, i, arg)?);
                i += 2;
            }
            "--description" | "-d" => {
                opts.description = Some(value(args, i, arg)?);
                i += 2;
            }
            "--url" | "-u" => {
                opts.url = Some(value(args, i, arg)?);
                i += 2;
            }
            "--license" | "-L" => {
                opts.license.extend(split_list(&value(args, i, arg)?));
                i += 2;
            }
            "--maintainer" | "-m" => {
                opts.maintainer = Some(value(args, i, arg)?);
                i += 2;
            }
            "--depends" | "-D" => {
                opts.depends.extend(split_list(&value(args, i, arg)?));
                i += 2;
            }
            "--provides" | "-P" => {
                opts.provides.extend(split_list(&value(args, i, arg)?));
                i += 2;
            }
            "--conflicts" => {
                opts.conflicts.extend(split_list(&value(args, i, arg)?));
                i += 2;
            }
            "--replaces" => {
                opts.replaces.extend(split_list(&value(args, i, arg)?));
                i += 2;
            }
            "--out" | "-o" => {
                opts.out = Some(PathBuf::from(value(args, i, arg)?));
                i += 2;
            }
            "--redfile" => {
                opts.redfile = Some(PathBuf::from(value(args, i, arg)?));
                i += 2;
            }
            "--native" => {
                opts.native = true;
                i += 1;
            }
            "--help" | "-h" => {
                if rebuild {
                    print_rebuild_help();
                } else {
                    print_build_help();
                }
                return Ok(None);
            }
            other if other.starts_with('-') => bail!("unknown build option '{other}'"),
            _ => {
                sources.push(args[i].clone());
                i += 1;
            }
        }
    }
    if sources.is_empty() {
        bail!(
            "usage: red -{} <source> [options] (run 'red -help {}')",
            if rebuild { "rebuild" } else { "build" },
            if rebuild { "rebuild" } else { "build" }
        );
    }
    Ok(Some(ParsedBuild { opts, sources }))
}

fn cmd_install(args: &[String]) -> Result<()> {
    let mut target: Option<String> = None;
    let mut root = default_root();
    let mut nodeps = false;
    let mut force = false;
    let mut run_hooks: Option<bool> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                root = PathBuf::from(value(args, i, "--root")?);
                i += 2;
            }
            "--nodeps" | "--no-deps" => {
                nodeps = true;
                i += 1;
            }
            "--force" | "-f" => {
                force = true;
                i += 1;
            }
            "--run-hooks" => {
                run_hooks = Some(true);
                i += 1;
            }
            "--no-hooks" => {
                run_hooks = Some(false);
                i += 1;
            }
            "--help" | "-h" => {
                println!("usage: red -install <.red|name> [--root DIR] [--nodeps] [--force] [--run-hooks|--no-hooks]");
                return Ok(());
            }
            other if other.starts_with('-') => bail!("unknown install option '{other}'"),
            _ => {
                if target.is_some() {
                    bail!("only one package can be installed at a time");
                }
                target = Some(args[i].clone());
                i += 1;
            }
        }
    }
    let target = target.ok_or_else(|| anyhow::anyhow!("usage: red -install <.red|name>"))?;
    fs::create_dir_all(&root)?;
    let opts = InstallOptions {
        run_hooks: run_hooks.unwrap_or_else(|| root == PathBuf::from("/")),
        root,
        nodeps,
        force,
    };
    install::install(&opts.root, &target, &opts)
}

fn cmd_remove(args: &[String]) -> Result<()> {
    let mut name: Option<String> = None;
    let mut root = default_root();
    let mut force = false;
    let mut run_hooks: Option<bool> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                root = PathBuf::from(value(args, i, "--root")?);
                i += 2;
            }
            "--force" | "-f" => {
                force = true;
                i += 1;
            }
            "--run-hooks" => {
                run_hooks = Some(true);
                i += 1;
            }
            "--no-hooks" => {
                run_hooks = Some(false);
                i += 1;
            }
            "--help" | "-h" => {
                println!("usage: red -remove <name> [--root DIR] [--force]");
                return Ok(());
            }
            other if other.starts_with('-') => bail!("unknown remove option '{other}'"),
            _ => {
                name = Some(args[i].clone());
                i += 1;
            }
        }
    }
    let name = name.ok_or_else(|| anyhow::anyhow!("usage: red -remove <name>"))?;
    let hooks = run_hooks.unwrap_or_else(|| root == PathBuf::from("/"));
    install::remove(&root, &name, force, hooks)
}

fn cmd_list(args: &[String]) -> Result<()> {
    let (root, _) = split_root(args)?;
    install::list_installed(&root)
}

fn cmd_info(args: &[String]) -> Result<()> {
    let (root, positional) = split_root(args)?;
    let target = positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: red -info <name|file.red>"))?;
    install::print_info(&root, target)
}

fn cmd_files(args: &[String]) -> Result<()> {
    let (root, positional) = split_root(args)?;
    let target = positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: red -files <name|file.red>"))?;
    install::print_files(&root, target)
}

fn cmd_verify(args: &[String]) -> Result<()> {
    let (_, positional) = split_root(args)?;
    let path = positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: red -verify <file.red>"))?;
    install::verify(PathBuf::from(path).as_path())
}

fn cmd_search(args: &[String]) -> Result<()> {
    let (root, positional) = split_root(args)?;
    let term = positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: red -search <term>"))?;
    install::search(&root, term)
}

fn cmd_repo(args: &[String]) -> Result<()> {
    let mut root = default_root();
    let mut sub: Option<String> = None;
    let mut path: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                root = PathBuf::from(value(args, i, "--root")?);
                i += 2;
            }
            "--help" | "-h" => {
                println!("usage: red -repo add|remove|list [DIR] [--root DIR]");
                return Ok(());
            }
            other if other.starts_with('-') => bail!("unknown repo option '{other}'"),
            _ => {
                if sub.is_none() {
                    sub = Some(args[i].clone());
                } else if path.is_none() {
                    path = Some(args[i].clone());
                } else {
                    bail!("unexpected argument '{}'", args[i]);
                }
                i += 1;
            }
        }
    }
    match sub.as_deref() {
        Some("add") => {
            let path = path.ok_or_else(|| anyhow::anyhow!("usage: red -repo add <dir>"))?;
            db::add_repo(&root, PathBuf::from(path).as_path())
        }
        Some("remove" | "rm") => {
            let path = path.ok_or_else(|| anyhow::anyhow!("usage: red -repo remove <dir>"))?;
            db::remove_repo(&root, PathBuf::from(path).as_path())
        }
        Some("list" | "ls") => install::list_repos(&root),
        _ => bail!("usage: red -repo add|remove|list [DIR]"),
    }
}

fn default_root() -> PathBuf {
    env::var_os("RED_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn split_root(args: &[String]) -> Result<(PathBuf, Vec<String>)> {
    let mut root = default_root();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--root" {
            root = PathBuf::from(value(args, i, "--root")?);
            i += 2;
        } else if args[i].starts_with('-') {
            bail!("unknown option '{}'", args[i]);
        } else {
            positional.push(args[i].clone());
            i += 1;
        }
    }
    Ok((root, positional))
}

fn value(args: &[String], index: usize, flag: &str) -> Result<String> {
    args.get(index + 1)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn cmd_gui(args: &[String]) -> Result<()> {
    let mut file: Option<PathBuf> = None;
    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("usage: red -gui [file]");
                return Ok(());
            }
            other if other.starts_with('-') => bail!("unknown gui option '{other}'"),
            other => {
                if file.is_some() {
                    bail!("only one file can be opened at a time");
                }
                file = Some(PathBuf::from(other));
            }
        }
    }
    let file = file.map(|path| fs::canonicalize(&path).unwrap_or(path));
    gui::run(file.as_deref())
}

fn print_usage() {
    println!(
        "RED {} - RED package manager\n\n\
Usage:\n  \
red -build <tar.*|dir>             Build a .red from native sources\n  \
red -rebuild <.deb|.rpm|.pkg.tar.*|PKGBUILD dir|tar.*|dir>\n\
                                     Convert a foreign package into .red\n  \
red -install <.red|.ared|name>    Install a package or a bundle\n  \
red -remove <name>                 Remove an installed package\n  \
red -list                          List installed packages\n  \
red -info <name|.red|.ared>        Show package information\n  \
red -files <name|.red>             List package files\n  \
red -verify <.red>                 Verify a package checksum\n  \
red -search <term>                 Search configured repositories\n  \
red -bundle <dir>                  Pack all .red in a directory into a .ared\n  \
red -repo add|remove|list [DIR]    Manage local repositories\n  \
red -gui [file]                    Open the graphical interface\n\n\
Sources for -build:\n  \
*.tar, *.tar.gz, *.tar.xz, *.tar.zst   Prebuilt file tree\n  \
directory with Redfile                 Directory payload\n\n\
Sources for -rebuild:\n  \
*.deb                    Convert with dpkg-deb   (requires dpkg)\n  \
*.rpm                    Convert with rpm2cpio  (requires rpm, cpio)\n  \
*.pkg.tar.*              Convert an Arch package\n  \
directory with PKGBUILD  Build with makepkg     (requires base-devel)\n  \
*.tar.* / directory      Same as -build\n\n\
Common options:\n  \
--root DIR       Install into an alternate root (default: / or $RED_ROOT)\n  \
--nodeps         Skip dependency checks\n  \
--force          Overwrite files / reinstall\n  \
--run-hooks      Run install hooks even under --root\n\n\
Examples:\n  \
red -build hello-1.0.0-x86_64.tar.xz --description 'Hello world'\n  \
red -build ./my-package          # uses ./my-package/Redfile\n  \
red -rebuild ./foo.deb\n  \
red -rebuild ./foo-1.0-1.x86_64.rpm\n  \
red -rebuild ./foo-1.0-1-x86_64.pkg.tar.zst\n  \
red -rebuild ./aur-package       # directory containing PKGBUILD\n  \
red -install ./hello-1.0.0-x86_64.red\n  \
red -repo add ./packages && red -install app\n\n\
Run 'red -help build' or 'red -help rebuild' for build options.",
        env!("CARGO_PKG_VERSION")
    );
}

fn print_build_help() {
    println!(
        "Usage: red -build <tar.*|directory> [options]\n\n\
Build a .red package from native sources (prebuilt tar archives or a directory).\n\
With --native the output is a pacman package (.pkg.tar.gz) for the current system\n\
instead of a .red. Self-contained app tarballs without a desktop entry get an\n\
opt/<name>/ layout plus a generated launcher menu entry automatically."
    );
    print_build_options();
}

fn print_rebuild_help() {
    println!(
        "Usage: red -rebuild <source> [options]\n\n\
Convert a foreign package into .red. The matching package manager is required:\n  \
.deb        dpkg-deb\n  \
.rpm        rpm, rpm2cpio, cpio\n  \
PKGBUILD    makepkg (base-devel)\n  \
.pkg.tar.*  none (Arch packages are tar archives)\n\n\
tar.* and directories are also accepted and behave like -build."
    );
    print_build_options();
}

fn print_build_options() {
    println!(
        "Options:\n  \
--name NAME            Package name\n  \
--version VERSION      Package version\n  \
--release N            Package release (default 1)\n  \
--arch ARCH            Architecture (default inferred or any)\n  \
--description TEXT     Description\n  \
--url URL              Homepage\n  \
--license LICENSE      License (repeatable / comma separated)\n  \
--maintainer NAME      Maintainer\n  \
--depends DEPS         Dependencies (repeatable / comma separated)\n  \
--provides PROVIDES    Provides (repeatable / comma separated)\n  \
--conflicts PKGS       Conflicts (repeatable / comma separated)\n  \
--replaces PKGS        Replaces (repeatable / comma separated)\n  \
--out FILE             Output .red path\n  \
--redfile FILE         Use a specific Redfile\n  \
--native               Build a pacman .pkg.tar.gz instead of .red\n\n\
A Redfile is optional JSON metadata placed next to the source (or in the\n\
source directory), for example:\n\
{{\n  \"name\": \"hello\", \"version\": \"1.0.0\", \"arch\": \"x86_64\",\n  \"description\": \"Hello world\", \"depends\": [\"libc\"],\n  \"hooks\": {{ \"post_install\": \"hooks/post-install.sh\" }}\n}}"
    );
}
