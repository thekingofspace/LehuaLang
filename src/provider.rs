use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{LehuaError, Result};

pub trait ModuleProvider: Send + Sync {
    fn base_dir(&self) -> &Path;
    fn exists(&self, id: &str) -> bool;
    fn read(&self, id: &str) -> Result<String>;
    fn binary_path(&self, id: &str) -> Result<PathBuf>;
}

pub struct FsProvider {
    root: PathBuf,
}

impl FsProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FsProvider { root: root.into() }
    }
    fn real(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
}

impl ModuleProvider for FsProvider {
    fn base_dir(&self) -> &Path {
        &self.root
    }
    fn exists(&self, id: &str) -> bool {
        self.real(id).is_file()
    }
    fn read(&self, id: &str) -> Result<String> {
        std::fs::read_to_string(self.real(id))
            .map_err(|e| LehuaError::msg(format!("could not read '{id}': {e}")))
    }
    fn binary_path(&self, id: &str) -> Result<PathBuf> {
        let candidates = [self.real(id), self.root.join(base_name(id))];
        first_existing(&candidates).ok_or_else(|| LehuaError::Dll {
            lib: id.to_string(),
            message: format!("native library not found at '{}'", self.real(id).display()),
        })
    }
}

fn base_name(id: &str) -> &str {
    id.rsplit(['/', '\\']).next().unwrap_or(id)
}

fn first_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.is_file()).cloned()
}

pub struct BundleProvider {
    base_dir: PathBuf,
    files: Arc<BTreeMap<String, String>>,
    dlls: Arc<BTreeMap<String, Vec<u8>>>,
    extract_dir: PathBuf,
    extracted: Mutex<BTreeMap<String, PathBuf>>,
    prefer_disk: bool,
}

impl BundleProvider {
    pub fn new(
        base_dir: impl Into<PathBuf>,
        files: Arc<BTreeMap<String, String>>,
        dlls: Arc<BTreeMap<String, Vec<u8>>>,
        extract_dir: impl Into<PathBuf>,
        prefer_disk: bool,
    ) -> Self {
        BundleProvider {
            base_dir: base_dir.into(),
            files,
            dlls,
            extract_dir: extract_dir.into(),
            extracted: Mutex::new(BTreeMap::new()),
            prefer_disk,
        }
    }
}

impl ModuleProvider for BundleProvider {
    fn base_dir(&self) -> &Path {
        &self.base_dir
    }
    fn exists(&self, id: &str) -> bool {
        self.files.contains_key(id)
    }
    fn read(&self, id: &str) -> Result<String> {
        self.files
            .get(id)
            .cloned()
            .ok_or_else(|| LehuaError::msg(format!("bundled module '{id}' is missing")))
    }
    fn binary_path(&self, id: &str) -> Result<PathBuf> {
        let mut extracted = self.extracted.lock().unwrap();
        if let Some(p) = extracted.get(id) {
            return Ok(p.clone());
        }
        if self.prefer_disk {
            let candidates = [self.base_dir.join(id), self.base_dir.join(base_name(id))];
            if let Some(p) = first_existing(&candidates) {
                return Ok(p);
            }
        }
        if let Some(bytes) = self.dlls.get(id) {
            std::fs::create_dir_all(&self.extract_dir)?;
            let out = self.extract_dir.join(id.replace(['/', '\\'], "__"));
            let needs_write = match std::fs::metadata(&out) {
                Ok(m) => m.len() != bytes.len() as u64,
                Err(_) => true,
            };
            if needs_write {
                let tmp = out.with_extension(format!("tmp.{}", std::process::id()));
                std::fs::write(&tmp, bytes)
                    .and_then(|_| std::fs::rename(&tmp, &out))
                    .map_err(|e| LehuaError::Dll {
                        lib: id.to_string(),
                        message: format!("failed to extract native library: {e}"),
                    })?;
            }
            extracted.insert(id.to_string(), out.clone());
            return Ok(out);
        }
        let candidates = [self.base_dir.join(id), self.base_dir.join(base_name(id))];
        if let Some(p) = first_existing(&candidates) {
            return Ok(p);
        }
        Err(LehuaError::Dll {
            lib: id.to_string(),
            message: format!(
                "native library was neither embedded nor found next to the executable (looked for '{}')",
                base_name(id)
            ),
        })
    }
}
