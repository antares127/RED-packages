use crate::manifest::Manifest;
use crate::util::{is_arch_metadata, sanitize_relative, sha256_file};
use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tar::{Archive, Builder, Header};
use tempfile::{Builder as TempBuilder, NamedTempFile};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Xz,
    Zstd,
}

impl Compression {
    pub fn name(self) -> &'static str {
        match self {
            Compression::None => "none",
            Compression::Gzip => "gzip",
            Compression::Xz => "xz",
            Compression::Zstd => "zstd",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Compression::None => "",
            Compression::Gzip => "gz",
            Compression::Xz => "xz",
            Compression::Zstd => "zst",
        }
    }

    pub fn from_name(name: &str) -> Result<Self> {
        match name.to_ascii_lowercase().as_str() {
            "" | "none" | "tar" => Ok(Compression::None),
            "gz" | "gzip" => Ok(Compression::Gzip),
            "xz" | "lzma2" => Ok(Compression::Xz),
            "zst" | "zstd" => Ok(Compression::Zstd),
            other => bail!("unsupported payload compression: {other}"),
        }
    }

    pub fn payload_name(self) -> String {
        if self == Compression::None {
            "payload.tar".to_string()
        } else {
            format!("payload.tar.{}", self.extension())
        }
    }

    pub fn detect_magic(path: &Path) -> Result<Option<Self>> {
        let mut file = File::open(path)?;
        let mut buf = [0u8; 6];
        let n = file.read(&mut buf)?;
        if n >= 2 && buf[0] == 0x1f && buf[1] == 0x8b {
            return Ok(Some(Compression::Gzip));
        }
        if n >= 6 && buf == [0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00] {
            return Ok(Some(Compression::Xz));
        }
        if n >= 4 && buf[..4] == [0x28, 0xb5, 0x2f, 0xfd] {
            return Ok(Some(Compression::Zstd));
        }
        Ok(None)
    }
}

/// Determine compression of a tar archive from its suffix, falling back to magic.
pub fn tar_compression(path: &Path) -> Result<Compression> {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    let by_suffix = if lower.ends_with(".tar") {
        Some(Compression::None)
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Some(Compression::Gzip)
    } else if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
        Some(Compression::Xz)
    } else if lower.ends_with(".tar.zst") || lower.ends_with(".tzst") {
        Some(Compression::Zstd)
    } else if lower.ends_with(".tar.bz2")
        || lower.ends_with(".tbz2")
        || lower.ends_with(".tar.lz")
        || lower.ends_with(".tar.lzma")
    {
        bail!(
            "unsupported compression for {}: only gzip, xz and zstd are supported",
            path.display()
        );
    } else {
        None
    };

    if let Some(comp) = by_suffix {
        return Ok(comp);
    }
    if let Some(comp) = Compression::detect_magic(path)? {
        return Ok(comp);
    }
    // A plain tar has "ustar" at offset 257.
    let mut file = File::open(path)?;
    let mut buf = [0u8; 265];
    let n = file.read(&mut buf)?;
    if n >= 262 && &buf[257..262] == b"ustar" {
        return Ok(Compression::None);
    }
    bail!(
        "cannot determine compression of {} (expected .tar, .tar.gz, .tar.xz or .tar.zst)",
        path.display()
    )
}

pub fn decompress_to(src: &Path, compression: Compression, dst: &Path) -> Result<()> {
    let input = BufReader::new(
        File::open(src).with_context(|| format!("cannot open {}", src.display()))?,
    );
    let output = BufWriter::new(
        File::create(dst).with_context(|| format!("cannot create {}", dst.display()))?,
    );
    match compression {
        Compression::None => {
            let mut input = input;
            let mut output = output;
            io::copy(&mut input, &mut output)?;
        }
        Compression::Gzip => {
            let mut decoder = GzDecoder::new(input);
            let mut output = output;
            io::copy(&mut decoder, &mut output)?;
        }
        Compression::Xz => {
            let mut input = input;
            let mut output = output;
            lzma_rs::xz_decompress(&mut input, &mut output)
                .map_err(|err| anyhow::anyhow!("xz decompression failed: {err:?}"))?;
        }
        Compression::Zstd => {
            let mut decoder = ruzstd::decoding::StreamingDecoder::new(input)
                .map_err(|err| anyhow::anyhow!("zstd decompression failed: {err:?}"))?;
            let mut output = output;
            io::copy(&mut decoder, &mut output)?;
        }
    }
    Ok(())
}

/// Decompress a payload into a temporary tar file.
pub fn decompress_to_temp(
    src: &Path,
    compression: Compression,
    temp_dir: &Path,
) -> Result<NamedTempFile> {
    let mut out = TempBuilder::new()
        .prefix("red-payload-")
        .suffix(".tar")
        .tempfile_in(temp_dir)?;
    decompress_to(src, compression, out.path())?;
    out.as_file_mut().flush()?;
    out.as_file_mut().seek(io::SeekFrom::Start(0))?;
    Ok(out)
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub files: Vec<String>,
    pub installed_size: u64,
}

pub fn scan_tar(tar_path: &Path, skip_arch_meta: bool) -> Result<ScanResult> {
    let file = File::open(tar_path)
        .with_context(|| format!("cannot open {}", tar_path.display()))?;
    let mut archive = Archive::new(BufReader::new(file));
    let mut result = ScanResult::default();

    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?.into_owned();
        let Some(rel) = sanitize_relative(&path)? else {
            continue;
        };
        let name = crate::util::rel_to_string(&rel)?;
        if skip_arch_meta && is_arch_metadata(&name) {
            continue;
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            result.files.push(format!("{name}/"));
        } else if entry_type.is_file() || entry_type.is_symlink() || entry_type.is_hard_link() {
            result.installed_size = result
                .installed_size
                .saturating_add(entry.header().size().unwrap_or(0));
            result.files.push(name);
        }
    }
    Ok(result)
}

/// Write a .red archive. `hooks` is keyed by archive member name.
pub fn write_red(
    out: &Path,
    manifest: &Manifest,
    payload: &Path,
    hooks: &BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    manifest.validate()?;
    let parent = out.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }
    let tmp = TempBuilder::new()
        .prefix(".red-build-")
        .tempfile_in(parent.unwrap_or(Path::new(".")))?;
    {
        let mut builder = Builder::new(tmp.as_file());
        append_bytes(&mut builder, "manifest.json", manifest.pretty().as_bytes(), 0o644)?;
        for (member, data) in hooks {
            append_bytes(&mut builder, member, data, 0o755)?;
        }

        let mut payload_file = File::open(payload)
            .with_context(|| format!("cannot open payload {}", payload.display()))?;
        let size = payload_file.metadata()?.len();
        let mut header = Header::new_gnu();
        header.set_path(&manifest.payload)?;
        header.set_size(size);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, &manifest.payload, &mut payload_file)
            .context("cannot append payload to .red archive")?;
        builder.finish().context("cannot finish .red archive")?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist(out)
        .map_err(|err| anyhow::anyhow!("cannot write {}: {}", out.display(), err.error))?;
    fs::set_permissions(out, fs::Permissions::from_mode(0o644))?;
    Ok(())
}

fn append_bytes(builder: &mut Builder<&File>, name: &str, data: &[u8], mode: u32) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_path(name)?;
    header.set_size(data.len() as u64);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    builder.append(&header, data)?;
    Ok(())
}

pub struct RedPackage {
    pub manifest: Manifest,
    pub payload: NamedTempFile,
    pub hooks: BTreeMap<String, Vec<u8>>,
}

impl RedPackage {
    pub fn payload_sha256(&self) -> Result<String> {
        sha256_file(self.payload.path())
    }

    pub fn verify(&self) -> Result<()> {
        let actual = self.payload_sha256()?;
        if !self.manifest.sha256.is_empty() && actual != self.manifest.sha256 {
            bail!(
                "payload checksum mismatch: expected {}, got {}",
                self.manifest.sha256,
                actual
            );
        }
        Ok(())
    }

    pub fn compression(&self) -> Result<Compression> {
        Compression::from_name(&self.manifest.compression)
    }

    /// Map phase -> hook bytes, using manifest.hooks member references.
    pub fn hook_scripts(&self) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut out = BTreeMap::new();
        for (phase, member) in &self.manifest.hooks {
            let data = self
                .hooks
                .get(member)
                .with_context(|| format!("hook member {member} missing from .red archive"))?
                .clone();
            out.insert(phase.clone(), data);
        }
        Ok(out)
    }
}

pub fn read_red(path: &Path, temp_dir: &Path) -> Result<RedPackage> {
    fs::create_dir_all(temp_dir)?;
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut archive = Archive::new(BufReader::new(file));
    let mut manifest: Option<Manifest> = None;
    let mut payload: Option<NamedTempFile> = None;
    let mut payload_member = String::new();
    let mut hooks: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path_in_archive = entry.path()?.into_owned();
        let name = path_in_archive.to_string_lossy().into_owned();
        if name == "manifest.json" {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            manifest = Some(
                serde_json::from_str(&text)
                    .context("invalid manifest.json inside .red archive")?,
            );
        } else if name.starts_with("hooks/") && !name.ends_with('/') {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            hooks.insert(name, data);
        } else if !name.ends_with('/') && payload.is_none() {
            let mut tmp = TempBuilder::new()
                .prefix("red-payload-")
                .tempfile_in(temp_dir)?;
            io::copy(&mut entry, tmp.as_file_mut())?;
            tmp.as_file_mut().flush()?;
            payload_member = name;
            payload = Some(tmp);
        }
    }

    let manifest = manifest.context("missing manifest.json inside .red archive")?;
    manifest.validate()?;
    let payload = payload.context("missing payload inside .red archive")?;
    if !payload_member.is_empty() && payload_member != manifest.payload {
        bail!(
            "payload member mismatch: manifest says {}, archive contains {}",
            manifest.payload,
            payload_member
        );
    }
    for member in manifest.hooks.values() {
        if !hooks.contains_key(member) {
            bail!("hook member {member} referenced by manifest is missing");
        }
    }

    Ok(RedPackage {
        manifest,
        payload,
        hooks,
    })
}

/// Read only the manifest, skipping the payload content.
pub fn read_manifest(path: &Path) -> Result<Manifest> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut archive = Archive::new(BufReader::new(file));
    for entry in archive.entries()? {
        let entry = entry?;
        if entry.path()?.to_string_lossy() == "manifest.json" {
            let mut text = String::new();
            let mut entry = entry;
            entry.read_to_string(&mut text)?;
            let manifest: Manifest =
                serde_json::from_str(&text).context("invalid manifest.json")?;
            manifest.validate()?;
            return Ok(manifest);
        }
    }
    bail!("{} is not a valid .red package (no manifest.json)", path.display());
}

pub fn temp_work_dir(root: &Path) -> Result<PathBuf> {
    let dir = root.join("var/tmp/red");
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn xz_payload_decompression_roundtrip() {
        let data = b"red xz payload test data";
        let mut compressed = Vec::new();
        lzma_rs::xz_compress(&mut BufReader::new(&data[..]), &mut compressed).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("payload.tar.xz");
        let dst = dir.path().join("payload.tar");
        fs::write(&src, &compressed).unwrap();
        decompress_to(&src, Compression::Xz, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), data);
    }

    #[test]
    fn gzip_payload_decompression_roundtrip() {
        use flate2::write::GzEncoder;
        use flate2::Compression as GzCompression;
        use std::io::Write;

        let data = b"red gzip payload test data";
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("payload.tar.gz");
        let dst = dir.path().join("payload.tar");
        let mut encoder = GzEncoder::new(File::create(&src).unwrap(), GzCompression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap();
        decompress_to(&src, Compression::Gzip, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), data);
    }
}
