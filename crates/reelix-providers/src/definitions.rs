//! Runtime indexer definition catalog: download (`.tar.gz`), safe extraction and loading.
//!
//! Layout under `{data_dir}/definitions`:
//! - `upstream/` — managed by [`crate::ProviderService::sync_definitions`]; replaced atomically
//!   (extracted to a temporary sibling directory, then renamed into place),
//! - `custom/` — user-provided files; a custom definition overrides an upstream one with the
//!   same id. Loose `*.yml` files found directly in `definitions/` are moved here on load.
//!
//! The upstream catalog has no license, so Reelix never ships it: it is fetched at runtime.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use reelix_api_types::providers::{DefinitionLoadErrorDto, DefinitionSource};
use reelix_indexers::DefinitionStore;

/// Maximum size of the downloaded archive.
pub const MAX_ARCHIVE_BYTES: usize = 50 * 1024 * 1024;
/// Maximum total bytes read from the decompressed tar stream (gzip-bomb guard).
pub const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum total size of the extracted definition files.
pub const MAX_EXTRACTED_BYTES: u64 = 50 * 1024 * 1024;
/// Maximum size of one definition file.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Maximum number of extracted definition files.
pub const MAX_FILES: usize = 10_000;

pub(crate) const UPSTREAM_DIR: &str = "upstream";
pub(crate) const CUSTOM_DIR: &str = "custom";

/// Result of loading `upstream/` + `custom/`.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    pub store: DefinitionStore,
    pub upstream_count: usize,
    pub custom_count: usize,
    pub overridden: usize,
    pub failed: Vec<DefinitionLoadErrorDto>,
}

fn is_definition_file(p: &Path) -> bool {
    p.is_file() && p.extension().is_some_and(|x| x == "yml" || x == "yaml")
}

fn definition_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| is_definition_file(p))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Number of definition files in `{root}/upstream`.
pub(crate) fn upstream_count(root: &Path) -> usize {
    definition_files(&root.join(UPSTREAM_DIR)).len()
}

/// Creates `upstream/` and `custom/` and moves loose definitions into `custom/`.
pub(crate) fn prepare_layout(root: &Path) {
    for d in [root.join(UPSTREAM_DIR), root.join(CUSTOM_DIR)] {
        if let Err(e) = std::fs::create_dir_all(&d) {
            tracing::warn!(dir = %d.display(), error = %e, "cannot create indexer definitions directory");
        }
    }
    for path in definition_files(root) {
        let Some(name) = path.file_name() else { continue };
        let target = root.join(CUSTOM_DIR).join(name);
        if target.exists() {
            tracing::warn!(path = %path.display(), "custom definition with the same name exists; leaving file in place");
            continue;
        }
        match std::fs::rename(&path, &target) {
            Ok(()) => {
                tracing::info!(from = %path.display(), to = %target.display(), "moved indexer definition to custom/")
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot move indexer definition to custom/")
            }
        }
    }
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Loads `custom/` then `upstream/`; custom ids win. Broken files are reported, not fatal.
pub(crate) fn load(root: &Path) -> Loaded {
    prepare_layout(root);
    let mut out = Loaded::default();
    let mut winners: Vec<(PathBuf, DefinitionSource, String)> = Vec::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut custom_ids: HashSet<String> = HashSet::new();
    let fail = |out: &mut Loaded, path: &Path, source, error: String| {
        tracing::warn!(path = %path.display(), %error, "skipping indexer definition");
        out.failed.push(DefinitionLoadErrorDto {
            id: stem(path),
            file: file_name(path),
            source,
            error,
        });
    };
    let custom = definition_files(&root.join(CUSTOM_DIR));
    let upstream = definition_files(&root.join(UPSTREAM_DIR));
    out.custom_count = custom.len();
    out.upstream_count = upstream.len();
    let custom_stems: HashSet<String> = custom.iter().map(|p| stem(p)).collect();
    for (files, source) in [
        (custom, DefinitionSource::Custom),
        (upstream, DefinitionSource::Upstream),
    ] {
        for path in files {
            if source == DefinitionSource::Upstream && custom_stems.contains(&stem(&path)) {
                out.overridden += 1;
                continue;
            }
            let yaml = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    fail(&mut out, &path, source, e.to_string());
                    continue;
                }
            };
            let def = match reelix_indexers::definition::parse(&yaml) {
                Ok(d) => d,
                Err(e) => {
                    fail(&mut out, &path, source, e);
                    continue;
                }
            };
            if ids.contains(&def.id) {
                if source == DefinitionSource::Upstream && custom_ids.contains(&def.id) {
                    out.overridden += 1;
                } else {
                    fail(
                        &mut out,
                        &path,
                        source,
                        format!("duplicate definition id `{}`", def.id),
                    );
                }
                continue;
            }
            if source == DefinitionSource::Custom {
                custom_ids.insert(def.id.clone());
            }
            ids.insert(def.id.clone());
            winners.push((path, source, yaml));
        }
    }
    for (path, source, yaml) in winners {
        if let Err(e) = out.store.add_yaml(&yaml) {
            fail(&mut out, &path, source, e.to_string());
        }
    }
    out
}

/// Files extracted from an archive.
#[derive(Debug)]
pub(crate) struct Extracted {
    pub files: usize,
    pub commit: Option<String>,
}

/// Errors out once more than `limit` bytes were read.
struct LimitedReader<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n as u64 > self.remaining {
            return Err(std::io::Error::other("archive is too large when decompressed"));
        }
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// The file name of an archive entry matching `<top>/definitions/<version>/<name>.yml`,
/// or `None` (other files, nested paths, `..`, absolute paths, odd names).
pub(crate) fn match_entry<'a>(path: &'a Path, version: &str) -> Option<&'a str> {
    let comps: Vec<Component<'_>> = path.components().collect();
    let [
        Component::Normal(_),
        Component::Normal(defs),
        Component::Normal(ver),
        Component::Normal(name),
    ] = comps.as_slice()
    else {
        return None;
    };
    if *defs != "definitions" || *ver != version {
        return None;
    }
    let name = name.to_str()?;
    let valid = name.len() <= 128
        && !name.starts_with('.')
        && name.ends_with(".yml")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    valid.then_some(name)
}

/// Extracts `*/definitions/{version}/*.yml` from a `.tar.gz` into `dest` (must exist).
/// Never uses the archive's paths for writing: only validated plain file names.
pub(crate) fn extract(archive: &[u8], version: &str, dest: &Path) -> Result<Extracted, String> {
    let gz = flate2::read::GzDecoder::new(archive);
    let reader = LimitedReader {
        inner: gz,
        remaining: MAX_UNPACKED_BYTES,
    };
    let mut tar = tar::Archive::new(reader);
    let mut files: HashSet<String> = HashSet::new();
    let mut total: u64 = 0;
    let mut commit = None;
    let entries = tar.entries().map_err(|e| format!("invalid archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("invalid archive: {e}"))?;
        let kind = entry.header().entry_type();
        if kind == tar::EntryType::XGlobalHeader {
            // GitHub archives record the commit id as the pax `comment`.
            if let Ok(Some(exts)) = entry.pax_extensions() {
                for ext in exts.flatten() {
                    if ext.key() == Ok("comment")
                        && let Ok(v) = ext.value()
                        && v.len() <= 64
                        && v.chars().all(|c| c.is_ascii_hexdigit())
                    {
                        commit = Some(v.to_owned());
                    }
                }
            }
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let path = match entry.path() {
            Ok(p) => p.into_owned(),
            Err(_) => continue,
        };
        let Some(name) = match_entry(&path, version) else {
            if path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::RootDir))
            {
                tracing::warn!(path = %path.display(), "ignoring unsafe path in definitions archive");
            }
            continue;
        };
        let size = entry.size();
        if size > MAX_FILE_BYTES {
            return Err(format!("{name} is larger than {MAX_FILE_BYTES} bytes"));
        }
        total += size;
        if total > MAX_EXTRACTED_BYTES {
            return Err(format!("definitions exceed {MAX_EXTRACTED_BYTES} bytes"));
        }
        if files.len() >= MAX_FILES {
            return Err(format!("archive contains more than {MAX_FILES} definitions"));
        }
        let mut buf = Vec::with_capacity(size as usize);
        (&mut entry)
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(|e| format!("invalid archive: {e}"))?;
        if buf.len() as u64 > MAX_FILE_BYTES {
            return Err(format!("{name} is larger than {MAX_FILE_BYTES} bytes"));
        }
        std::fs::write(dest.join(name), &buf).map_err(|e| format!("cannot write {name}: {e}"))?;
        files.insert(name.to_owned());
    }
    Ok(Extracted {
        files: files.len(),
        commit,
    })
}

/// Extracts `archive` and atomically replaces `{root}/upstream`. On error the previous
/// catalog is kept.
pub(crate) fn install(root: &Path, archive: &[u8], version: &str) -> Result<Extracted, String> {
    prepare_layout(root);
    let suffix = format!("{:016x}", rand::random::<u64>());
    let tmp = root.join(format!(".upstream-new-{suffix}"));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    let res = extract(archive, version, &tmp).and_then(|x| {
        if x.files == 0 {
            Err(format!(
                "the archive contains no definitions under definitions/{version}/"
            ))
        } else {
            Ok(x)
        }
    });
    let extracted = match res {
        Ok(x) => x,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    let upstream = root.join(UPSTREAM_DIR);
    let old = root.join(format!(".upstream-old-{suffix}"));
    let had_old = upstream.exists();
    if had_old && let Err(e) = std::fs::rename(&upstream, &old) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("cannot replace {}: {e}", upstream.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, &upstream) {
        if had_old {
            let _ = std::fs::rename(&old, &upstream);
        }
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("cannot install definitions: {e}"));
    }
    if had_old {
        let _ = std::fs::remove_dir_all(&old);
    }
    // Leftovers from interrupted syncs.
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.filter_map(Result::ok) {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if (n.starts_with(".upstream-new-") || n.starts_with(".upstream-old-")) && e.path().is_dir() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(extracted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_matching() {
        let ok = |p: &str| match_entry(Path::new(p), "v11").map(str::to_owned);
        assert_eq!(
            ok("Indexers-master/definitions/v11/foo.yml").as_deref(),
            Some("foo.yml")
        );
        assert_eq!(ok("Indexers-master/definitions/v10/foo.yml"), None);
        assert_eq!(ok("Indexers-master/definitions/v11/sub/foo.yml"), None);
        assert_eq!(ok("Indexers-master/definitions/v11/../foo.yml"), None);
        assert_eq!(ok("../definitions/v11/foo.yml"), None);
        assert_eq!(ok("/x/definitions/v11/foo.yml"), None);
        assert_eq!(ok("Indexers-master/definitions/v11/.hidden.yml"), None);
        assert_eq!(ok("Indexers-master/definitions/v11/foo.yaml"), None);
        assert_eq!(ok("Indexers-master/definitions/v11/f o.yml"), None);
    }
}
