use std::collections::{BTreeMap, HashSet};

use crate::error::{LehuaError, Result};
use crate::manifest::LibManifest;
use crate::provider::ModuleProvider;
use crate::vpath;

#[derive(Debug, Clone)]
pub enum Resolved {
    Builtin(String),
    Module(String),
}

fn rel_base(from_id: &str) -> String {
    let dir = vpath::dirname(from_id);
    if from_id == "init.luau" || from_id.ends_with("/init.luau") {
        vpath::dirname(&dir)
    } else {
        dir
    }
}

fn chain_label(chain: &[String], from_id: &str) -> String {
    if chain.is_empty() {
        from_id.to_string()
    } else {
        chain.join(" -> ")
    }
}

pub struct Resolver {
    pub aliases: BTreeMap<String, String>,
    pub roots_dir: String,
    pub included: HashSet<String>,
    pub known_builtins: HashSet<String>,
}

impl Resolver {
    pub fn build(
        aliases_raw: &BTreeMap<String, String>,
        roots_dir: String,
        included: HashSet<String>,
    ) -> Resolver {
        let aliases = aliases_raw
            .iter()
            .map(|(k, v)| (k.clone(), vpath::normalize(v)))
            .collect();
        let known_builtins = crate::libs::KNOWN.iter().map(|s| s.to_string()).collect();
        Resolver {
            aliases,
            roots_dir: vpath::normalize(&roots_dir),
            included,
            known_builtins,
        }
    }

    pub fn resolve(
        &self,
        from_id: &str,
        request: &str,
        provider: &dyn ModuleProvider,
    ) -> Result<Resolved> {
        let req = request.trim();

        if req.starts_with("./") || req.starts_with("../") {
            let target = vpath::join(&rel_base(from_id), req);
            return self.resolve_file(provider, &target, from_id, request);
        }

        if req == "@self" || req.starts_with("@self/") {
            let base = vpath::dirname(from_id);
            let rest = req.trim_start_matches("@self").trim_start_matches('/');
            let target = if rest.is_empty() {
                base
            } else {
                vpath::join(&base, rest)
            };
            return self.resolve_file(provider, &target, from_id, request);
        }

        if let Some(rest) = req.strip_prefix('@') {
            let (name, sub) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (rest, ""),
            };
            let dir = self.aliases.get(name).ok_or_else(|| LehuaError::ModuleNotFound {
                name: request.to_string(),
                from: from_id.to_string(),
            })?;
            let target = if sub.is_empty() {
                vpath::normalize(dir)
            } else {
                vpath::join(dir, sub)
            };
            return self.resolve_file(provider, &target, from_id, request);
        }

        if self.known_builtins.contains(req) {
            if self.included.contains(req) {
                return Ok(Resolved::Builtin(req.to_string()));
            }
            return Err(LehuaError::msg(format!(
                "library '{req}' is required but not included - add `--#include[{req}]` to a source file"
            )));
        }

        self.resolve_root(provider, req)
    }

    pub fn resolve_chain(
        &self,
        chain: &[String],
        from_id: &str,
        request: &str,
        provider: &dyn ModuleProvider,
    ) -> Result<Resolved> {
        let req = request.trim();
        if req.starts_with("./") || req.starts_with("../") {
            for from in chain {
                let target = vpath::join(&rel_base(from), req);
                if let Ok(r) = self.resolve_file(provider, &target, from, request) {
                    return Ok(r);
                }
            }
            return Err(LehuaError::ModuleNotFound {
                name: request.to_string(),
                from: chain_label(chain, from_id),
            });
        }
        self.resolve(from_id, request, provider)
    }

    pub fn resolve_worker(
        &self,
        chain: &[String],
        from_id: &str,
        path: &str,
        provider: &dyn ModuleProvider,
    ) -> Result<String> {
        let p = path.trim();
        if p.starts_with('@') {
            return match self.resolve(from_id, p, provider)? {
                Resolved::Module(id) => Ok(id),
                Resolved::Builtin(_) => Err(LehuaError::ModuleNotFound {
                    name: path.to_string(),
                    from: from_id.to_string(),
                }),
            };
        }
        for from in chain {
            let target = vpath::join(&rel_base(from), p);
            if let Ok(Resolved::Module(id)) = self.resolve_file(provider, &target, from, path) {
                return Ok(id);
            }
        }
        Err(LehuaError::ModuleNotFound {
            name: path.to_string(),
            from: chain_label(chain, from_id),
        })
    }

    fn resolve_root(&self, provider: &dyn ModuleProvider, name: &str) -> Result<Resolved> {
        let dir = vpath::join(&self.roots_dir, name);
        let lib_toml = format!("{dir}/lib.toml");
        let info = if provider.exists(&lib_toml) {
            let text = provider.read(&lib_toml)?;
            let lib: LibManifest = toml::from_str(&text).map_err(|e| {
                LehuaError::msg(format!("root '{name}': invalid lib.toml: {e}"))
            })?;
            Some(lib.root)
        } else {
            None
        };
        let entry = info
            .as_ref()
            .map(|r| r.entry.clone())
            .unwrap_or_else(|| "init.luau".to_string());
        let target = vpath::join(&dir, &entry);
        if provider.exists(&target) {
            Ok(Resolved::Module(target))
        } else if let Some(r) = info {
            let desc = if r.description.is_empty() {
                String::new()
            } else {
                format!(" - {}", r.description)
            };
            Err(LehuaError::msg(format!(
                "root '{}' v{}{desc}: entry '{}' not found under '{dir}'",
                r.name, r.version, r.entry
            )))
        } else {
            Err(LehuaError::RootNotFound(name.to_string(), self.roots_dir.clone()))
        }
    }

    fn resolve_file(
        &self,
        provider: &dyn ModuleProvider,
        target: &str,
        from_id: &str,
        request: &str,
    ) -> Result<Resolved> {
        let candidates = [
            target.to_string(),
            format!("{target}.luau"),
            format!("{target}/init.luau"),
        ];
        for cand in candidates {
            let norm = vpath::normalize(&cand);
            if provider.exists(&norm) {
                return Ok(Resolved::Module(norm));
            }
        }
        Err(LehuaError::ModuleNotFound {
            name: request.to_string(),
            from: from_id.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    struct MapProvider {
        base: PathBuf,
        files: Vec<String>,
    }

    impl ModuleProvider for MapProvider {
        fn base_dir(&self) -> &Path {
            &self.base
        }
        fn exists(&self, id: &str) -> bool {
            self.files.iter().any(|f| f == id)
        }
        fn read(&self, id: &str) -> Result<String> {
            Err(LehuaError::msg(format!("no content for '{id}'")))
        }
        fn binary_path(&self, id: &str) -> Result<PathBuf> {
            Err(LehuaError::msg(format!("no binary for '{id}'")))
        }
    }

    fn provider(files: &[&str]) -> MapProvider {
        MapProvider {
            base: PathBuf::from("."),
            files: files.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn resolver() -> Resolver {
        Resolver::build(&BTreeMap::new(), "roots".to_string(), HashSet::new())
    }

    fn module_id(r: Result<Resolved>) -> String {
        match r.unwrap() {
            Resolved::Module(id) => id,
            Resolved::Builtin(b) => panic!("expected module, got builtin '{b}'"),
        }
    }

    #[test]
    fn init_relative_points_at_folder_parent() {
        let p = provider(&["pkg/init.luau", "pkg/child.luau", "sibling.luau"]);
        let r = resolver();
        assert_eq!(
            module_id(r.resolve("pkg/init.luau", "./sibling", &p)),
            "sibling.luau"
        );
    }

    #[test]
    fn init_self_points_at_own_folder() {
        let p = provider(&["pkg/init.luau", "pkg/child.luau"]);
        let r = resolver();
        assert_eq!(
            module_id(r.resolve("pkg/init.luau", "@self/child", &p)),
            "pkg/child.luau"
        );
    }

    #[test]
    fn nested_init_relative_and_parent() {
        let p = provider(&["a/pkg/init.luau", "a/log.luau", "shared/util.luau"]);
        let r = resolver();
        assert_eq!(
            module_id(r.resolve("a/pkg/init.luau", "./log", &p)),
            "a/log.luau"
        );
        assert_eq!(
            module_id(r.resolve("a/pkg/init.luau", "../shared/util", &p)),
            "shared/util.luau"
        );
    }

    #[test]
    fn plain_file_relative_points_at_own_folder() {
        let p = provider(&["pkg/util.luau", "pkg/other.luau"]);
        let r = resolver();
        assert_eq!(
            module_id(r.resolve("pkg/util.luau", "./other", &p)),
            "pkg/other.luau"
        );
    }

    #[test]
    fn relative_can_load_folder_init() {
        let p = provider(&["main.luau", "thing/init.luau"]);
        let r = resolver();
        assert_eq!(
            module_id(r.resolve("main.luau", "./thing", &p)),
            "thing/init.luau"
        );
    }
}
