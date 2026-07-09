use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::dll;
use crate::error::{LehuaError, Result};
use crate::headers;
use crate::manifest::{BuildManifest, LuauRc};
use crate::provider::{FsProvider, ModuleProvider};
use crate::resolver::{Resolved, Resolver};
use crate::vpath;

const MAGIC: &[u8; 16] = b"LEHUA\0BUNDLE\0v1\0";
const TRAILER_LEN: u64 = 24;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    pub name: String,
    pub version: String,
    pub entry: String,
    pub includes: Vec<String>,
    pub aliases: BTreeMap<String, String>,
    pub roots_dir: String,
    pub language_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub manifest: BundleManifest,
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub dlls_b64: BTreeMap<String, String>,
}

impl Bundle {
    pub fn dll_bytes(&self) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        for (id, b64) in &self.dlls_b64 {
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
                out.insert(id.clone(), bytes);
            }
        }
        out
    }
}

pub struct BuildReport {
    pub bundle: Bundle,
    pub module_count: usize,
    pub dll_count: usize,
    pub notes: Vec<String>,
}

pub fn build_bundle(
    project_root: &Path,
    manifest: &BuildManifest,
    luaurc: &LuauRc,
) -> Result<BuildReport> {
    let provider = FsProvider::new(project_root.to_path_buf());
    let roots_dir = vpath::normalize(&manifest.roots.path);

    let all_builtins: HashSet<String> =
        crate::libs::KNOWN.iter().map(|s| s.to_string()).collect();
    let resolver = Resolver::build(&luaurc.aliases, roots_dir.clone(), all_builtins);

    let entry_id = vpath::normalize(&manifest.project.entry);
    if !provider.exists(&entry_id) {
        return Err(LehuaError::msg(format!(
            "entry module '{entry_id}' does not exist"
        )));
    }

    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut dlls_b64: BTreeMap<String, String> = BTreeMap::new();
    let mut includes: Vec<String> = Vec::new();
    let mut required_builtins: HashSet<String> = HashSet::new();
    let mut notes: Vec<String> = Vec::new();

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = vec![entry_id.clone()];

    while let Some(id) = queue.pop() {
        if is_type_def(&id) || !visited.insert(id.clone()) {
            continue;
        }
        let source = provider.read(&id)?;
        files.insert(id.clone(), source.clone());

        if let Some(lib_toml) = root_lib_toml(&roots_dir, &id) {
            if provider.exists(&lib_toml) && !files.contains_key(&lib_toml) {
                files.insert(lib_toml.clone(), provider.read(&lib_toml)?);
            }
        }

        let directives = headers::parse(&source);
        for inc in &directives.includes {
            if inc == "all" || inc == "*" {
                for k in crate::libs::KNOWN {
                    let k = (*k).to_string();
                    if !includes.contains(&k) {
                        includes.push(k);
                    }
                }
                continue;
            }
            if !crate::libs::KNOWN.contains(&inc.as_str()) {
                return Err(LehuaError::msg(format!(
                    "'{id}': unknown library '{inc}' in --#include (known: {}, or 'all')",
                    crate::libs::KNOWN.join(", ")
                )));
            }
            if !includes.contains(inc) {
                includes.push(inc.clone());
            }
        }
        for inj in &directives.injects {
            let did = dll::dll_id(&id, inj);
            if dlls_b64.contains_key(&did) {
                continue;
            }
            match provider.binary_path(&did) {
                Ok(path) => match std::fs::read(&path) {
                    Ok(bytes) => {
                        dlls_b64.insert(
                            did.clone(),
                            base64::engine::general_purpose::STANDARD.encode(&bytes),
                        );
                    }
                    Err(e) => notes.push(format!("could not read DLL '{did}': {e}")),
                },
                Err(_) => notes.push(format!(
                    "DLL '{did}' not found at build time - it was not embedded; \
                     the exe will look for it next to itself at runtime"
                )),
            }
        }

        for (kind, literal) in scan_calls(&source) {
            if kind == DepKind::Dll {
                let did = dll::dll_id(&id, &literal);
                if dlls_b64.contains_key(&did) {
                    continue;
                }
                match provider.binary_path(&did) {
                    Ok(path) => match std::fs::read(&path) {
                        Ok(bytes) => {
                            dlls_b64.insert(
                                did.clone(),
                                base64::engine::general_purpose::STANDARD.encode(&bytes),
                            );
                        }
                        Err(e) => notes.push(format!("could not read DLL '{did}': {e}")),
                    },
                    Err(_) => notes.push(format!(
                        "DLL '{did}' (from dll.open) not found at build time - it was not embedded; \
                         the exe will look for it next to itself at runtime"
                    )),
                }
                continue;
            }
            let resolved = if kind == DepKind::Parallel {
                resolver
                    .resolve_worker(&vpath::dirname(&id), &literal, &provider)
                    .map(Resolved::Module)
            } else {
                resolver.resolve(&id, &literal, &provider)
            };
            match resolved {
                Ok(Resolved::Module(mid)) => queue.push(mid),
                Ok(Resolved::Builtin(name)) => {
                    required_builtins.insert(name);
                }
                Err(e) => notes.push(format!(
                    "in '{id}': could not statically resolve {} \"{literal}\" ({e}); not bundled",
                    kind.as_str()
                )),
            }
        }
    }

    for b in &required_builtins {
        if !includes.contains(b) {
            return Err(LehuaError::msg(format!(
                "built-in library '{b}' is used but never included - add `--#include[{b}]` to a source file"
            )));
        }
    }

    let aliases: BTreeMap<String, String> = luaurc
        .aliases
        .iter()
        .map(|(k, v)| (k.clone(), vpath::normalize(v)))
        .collect();

    let module_count = files.len();
    let dll_count = dlls_b64.len();

    let bundle = Bundle {
        manifest: BundleManifest {
            name: manifest.project.name.clone(),
            version: manifest.project.version.clone(),
            entry: entry_id,
            includes,
            aliases,
            roots_dir,
            language_mode: luaurc
                .language_mode
                .clone()
                .unwrap_or_else(|| "nonstrict".to_string()),
        },
        files,
        dlls_b64,
    };

    Ok(BuildReport {
        bundle,
        module_count,
        dll_count,
        notes,
    })
}

fn is_type_def(id: &str) -> bool {
    id.ends_with(".d.luau")
}

fn root_lib_toml(roots_dir: &str, id: &str) -> Option<String> {
    let prefix = format!("{roots_dir}/");
    let rest = id.strip_prefix(&prefix)?;
    let name = rest.split('/').next()?;
    Some(format!("{roots_dir}/{name}/lib.toml"))
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum DepKind {
    Require,
    Parallel,
    Dll,
}

impl DepKind {
    fn as_str(self) -> &'static str {
        match self {
            DepKind::Require => "require",
            DepKind::Parallel => "parallel",
            DepKind::Dll => "dll.open",
        }
    }
}

fn scan_calls(src: &str) -> Vec<(DepKind, String)> {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut out = Vec::new();

    while i < n {
        let c = b[i];
        if c == b'-' && i + 1 < n && b[i + 1] == b'-' {
            i += 2;
            if i < n && b[i] == b'[' {
                if let Some(level) = long_bracket_level(b, i) {
                    i = skip_long_bracket(b, i, level);
                    continue;
                }
            }
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'"' || c == b'\'' || c == b'`' {
            i = skip_quoted(b, i);
            continue;
        }
        if c == b'[' {
            if let Some(level) = long_bracket_level(b, i) {
                i = skip_long_bracket(b, i, level);
                continue;
            }
        }
        if is_ident_start(c) {
            let start = i;
            while i < n && is_ident_part(b[i]) {
                i += 1;
            }
            let word = &src[start..i];
            if word == "dll" && !is_member_access(b, start) {
                let mut j = skip_ws(b, i);
                if j < n && b[j] == b'.' {
                    j = skip_ws(b, j + 1);
                    let method_start = j;
                    while j < n && is_ident_part(b[j]) {
                        j += 1;
                    }
                    let method = &src[method_start..j];
                    if method == "open" || method == "load" {
                        let mut k = skip_ws(b, j);
                        if k < n && b[k] == b'(' {
                            k = skip_ws(b, k + 1);
                        }
                        if let Some((literal, end)) = read_string_arg(b, k) {
                            out.push((DepKind::Dll, literal));
                            i = end;
                            continue;
                        }
                    }
                }
            }
            let kind = match word {
                "require" => Some(DepKind::Require),
                "parallel" => Some(DepKind::Parallel),
                _ => None,
            };
            if let Some(kind) = kind {
                if !is_member_access(b, start) {
                    let mut j = skip_ws(b, i);
                    if j < n && b[j] == b'(' {
                        j = skip_ws(b, j + 1);
                    }
                    if let Some((literal, end)) = read_string_arg(b, j) {
                        out.push((kind, literal));
                        i = end;
                        continue;
                    }
                }
            }
            continue;
        }
        i += 1;
    }
    out
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}
fn is_ident_part(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn is_member_access(b: &[u8], start: usize) -> bool {
    let mut p = start;
    while p > 0 && b[p - 1].is_ascii_whitespace() {
        p -= 1;
    }
    if p == 0 {
        return false;
    }
    match b[p - 1] {
        b':' => true,
        b'.' => !(p >= 2 && b[p - 2] == b'.'),
        _ => false,
    }
}

fn read_string_arg(b: &[u8], pos: usize) -> Option<(String, usize)> {
    match b.get(pos)? {
        b'"' | b'\'' | b'`' => read_literal(b, pos),
        b'[' => {
            let level = long_bracket_level(b, pos)?;
            let content_start = pos + 2 + level;
            let end = skip_long_bracket(b, pos, level);
            let close_len = 2 + level;
            let content_end = end.saturating_sub(close_len).max(content_start);
            let bytes = &b[content_start..content_end.min(b.len())];
            Some((String::from_utf8_lossy(bytes).into_owned(), end))
        }
        _ => None,
    }
}

fn skip_quoted(b: &[u8], start: usize) -> usize {
    let quote = b[start];
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            c if c == quote => return i + 1,
            b'\n' if quote != b'`' => return i + 1,
            _ => i += 1,
        }
    }
    i
}

fn read_literal(b: &[u8], start: usize) -> Option<(String, usize)> {
    let quote = b[start];
    let mut i = start + 1;
    let mut bytes: Vec<u8> = Vec::new();
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() => {
                bytes.push(b[i + 1]);
                i += 2;
            }
            c if c == quote => return Some((String::from_utf8_lossy(&bytes).into_owned(), i + 1)),
            b'\n' if quote != b'`' => return None,
            c => {
                bytes.push(c);
                i += 1;
            }
        }
    }
    None
}

fn long_bracket_level(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = i + 1;
    let mut level = 0;
    while b.get(j) == Some(&b'=') {
        level += 1;
        j += 1;
    }
    if b.get(j) == Some(&b'[') {
        Some(level)
    } else {
        None
    }
}

fn skip_long_bracket(b: &[u8], i: usize, level: usize) -> usize {
    let mut j = i + 2 + level;
    let close_len = 2 + level;
    while j + close_len <= b.len() {
        if b[j] == b']' {
            let mut k = j + 1;
            let mut eqs = 0;
            while b.get(k) == Some(&b'=') {
                eqs += 1;
                k += 1;
            }
            if eqs == level && b.get(k) == Some(&b']') {
                return k + 1;
            }
        }
        j += 1;
    }
    b.len()
}

pub fn serialize_payload(bundle: &Bundle) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(bundle)?;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(&json)?;
    Ok(enc.finish()?)
}

pub fn write_executable(runtime_exe: &Path, payload: &[u8], out_exe: &Path) -> Result<()> {
    let runtime_abs = std::fs::canonicalize(runtime_exe).unwrap_or_else(|_| runtime_exe.to_path_buf());
    let out_abs = std::path::absolute(out_exe).unwrap_or_else(|_| out_exe.to_path_buf());
    if runtime_abs == out_abs {
        return Err(LehuaError::msg(
            "refusing to build: output path is the running runtime itself",
        ));
    }
    let mut bytes = std::fs::read(runtime_exe)?;
    if read_trailer_bytes(&bytes).is_some() {
        return Err(LehuaError::msg(format!(
            "runtime '{}' already contains an embedded app and cannot be used as a build runtime",
            runtime_exe.display()
        )));
    }
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(MAGIC);
    if let Some(parent) = out_exe.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_exe, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(out_exe)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(out_exe, perms)?;
    }
    Ok(())
}

fn read_trailer_bytes(data: &[u8]) -> Option<u64> {
    if (data.len() as u64) < TRAILER_LEN {
        return None;
    }
    let tail = &data[data.len() - TRAILER_LEN as usize..];
    if &tail[8..] != MAGIC {
        return None;
    }
    Some(u64::from_le_bytes(tail[..8].try_into().ok()?))
}

pub fn load_embedded() -> Result<Option<Bundle>> {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    let mut f = match std::fs::File::open(&exe) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    let len = f.metadata()?.len();
    if len < TRAILER_LEN {
        return Ok(None);
    }
    f.seek(SeekFrom::End(-(TRAILER_LEN as i64)))?;
    let mut tail = [0u8; TRAILER_LEN as usize];
    f.read_exact(&mut tail)?;
    if &tail[8..] != MAGIC {
        return Ok(None);
    }
    let plen = u64::from_le_bytes(tail[..8].try_into().unwrap());
    if plen == 0 || plen > len - TRAILER_LEN {
        return Err(LehuaError::msg("embedded payload length is invalid"));
    }
    f.seek(SeekFrom::End(-((plen + TRAILER_LEN) as i64)))?;
    let mut payload = vec![0u8; plen as usize];
    f.read_exact(&mut payload)?;
    let mut dec = flate2::read::GzDecoder::new(&payload[..]);
    let mut json = Vec::new();
    dec.read_to_end(&mut json)?;
    Ok(Some(serde_json::from_slice(&json)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deps(src: &str) -> Vec<(&'static str, String)> {
        scan_calls(src)
            .into_iter()
            .map(|(k, s)| (k.as_str(), s))
            .collect()
    }

    #[test]
    fn scans_basic_requires() {
        assert_eq!(deps(r#"local a = require("./a")"#), vec![("require", "./a".to_string())]);
        assert_eq!(deps(r#"parallel('./w.luau')"#), vec![("parallel", "./w.luau".to_string())]);
    }

    #[test]
    fn ignores_member_calls_and_strings() {
        assert!(deps(r#"obj.require("x")"#).is_empty());
        assert!(deps(r#"t:require("x")"#).is_empty());
        assert!(deps(r#"local s = "require('x')""#).is_empty());
        assert!(deps("local s = `require('{x}')`").is_empty());
        assert!(deps("-- require('x')").is_empty());
        assert!(deps("--[[ require('x') ]]").is_empty());
    }

    #[test]
    fn scans_dll_opens() {
        assert_eq!(
            deps(r#"local m = dll.open("./native/m.dll")"#),
            vec![("dll.open", "./native/m.dll".to_string())]
        );
        assert_eq!(deps(r#"dll.load 'x.dll'"#), vec![("dll.open", "x.dll".to_string())]);
        assert!(deps(r#"foo.dll.open("./m.dll")"#).is_empty());
        assert!(deps(r#"dll.open(path)"#).is_empty());
    }

    #[test]
    fn keeps_concat_and_parenless_and_longbracket() {
        assert_eq!(deps(r#"x .. require("y")"#), vec![("require", "y".to_string())]);
        assert_eq!(deps(r#"require "z""#), vec![("require", "z".to_string())]);
        assert_eq!(deps(r#"require([[mod]])"#), vec![("require", "mod".to_string())]);
    }
}
