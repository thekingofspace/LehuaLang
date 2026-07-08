use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::Result;

fn default_version() -> String {
    "0.1.0".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuildManifest {
    pub project: Project,
    #[serde(default)]
    pub build: BuildSection,
    #[serde(default)]
    pub roots: RootsSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    pub entry: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuildSection {
    #[serde(default = "BuildSection::default_output")]
    pub output: String,
}

impl BuildSection {
    fn default_output() -> String {
        "dist".to_string()
    }
}

impl Default for BuildSection {
    fn default() -> Self {
        BuildSection {
            output: Self::default_output(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RootsSection {
    #[serde(default = "RootsSection::default_path")]
    pub path: String,
}

impl RootsSection {
    fn default_path() -> String {
        "roots".to_string()
    }
}

impl Default for RootsSection {
    fn default() -> Self {
        RootsSection {
            path: Self::default_path(),
        }
    }
}

impl BuildManifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibManifest {
    pub root: RootInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RootInfo {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "RootInfo::default_entry")]
    pub entry: String,
}

impl RootInfo {
    fn default_entry() -> String {
        "init.luau".to_string()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LuauRc {
    #[serde(default, rename = "languageMode")]
    pub language_mode: Option<String>,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
}

impl LuauRc {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let stripped = strip_jsonc_comments(&text);
        Ok(serde_json::from_str(&stripped)?)
    }
}

fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                while let Some(&n) = chars.peek() {
                    if n == '\n' {
                        break;
                    }
                    chars.next();
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}
