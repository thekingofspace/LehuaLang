use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::bundle::{self, Bundle};
use crate::engine::{self, Engine};
use crate::error::{LehuaError, Result};
use crate::manifest::{BuildManifest, LuauRc};
use crate::portable::PortableValue;
use crate::provider::BundleProvider;
use crate::resolver::Resolver;
use crate::scaffold;

#[derive(Parser)]
#[command(
    name = "lehua",
    version,
    about = "Lehua — the next-generation Luau runtime (compiled, multithreaded, batteries-included)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(about = "Compile (tree-shake) and run a project or a single .luau file")]
    Run {
        #[arg(help = "Path to a .luau file or a project directory (defaults to ./build.toml)")]
        path: Option<String>,
        #[arg(
            last = true,
            allow_hyphen_values = true,
            help = "Arguments passed through to the program (after --)"
        )]
        program_args: Vec<String>,
    },
    #[command(about = "Build a standalone executable that embeds the whole app")]
    Build {
        #[arg(long, help = "Output path for the executable (overrides build.toml)")]
        out: Option<String>,
        #[arg(
            long,
            help = "Lehua runtime binary to embed into (for cross-building, e.g. a Linux runtime); defaults to this executable"
        )]
        runtime: Option<String>,
    },
    #[command(about = "Remove the build cache and dist output")]
    Clean,
    #[command(about = "Scaffold a new Lehua project")]
    Init {
        #[arg(help = "Project directory to create (defaults to the current directory)")]
        name: Option<String>,
    },
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Run { path, program_args } => cmd_run(path, program_args),
        Cmd::Build { out, runtime } => cmd_build(out, runtime),
        Cmd::Clean => cmd_clean(),
        Cmd::Init { name } => scaffold::init(name),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lehua: {e}");
            ExitCode::FAILURE
        }
    }
}

pub fn run_embedded(bundle: Bundle, args: Vec<String>) -> ExitCode {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let extract = std::env::temp_dir().join(format!(
        "lehua-{}-{}",
        bundle.manifest.name, bundle.manifest.version
    ));
    match execute_bundle(bundle, exe_dir, extract, args, true) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lehua: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_run(path: Option<String>, program_args: Vec<String>) -> Result<()> {
    let (root, manifest, luaurc) = load_project(path)?;
    let report = bundle::build_bundle(&root, &manifest, &luaurc)?;
    for note in &report.notes {
        eprintln!("lehua: note: {note}");
    }
    let extract = root.join(".lehua").join("cache");
    execute_bundle(report.bundle, root, extract, program_args, false)
}

fn cmd_build(out: Option<String>, runtime: Option<String>) -> Result<()> {
    let root = absolutize(Path::new("."));
    let manifest = BuildManifest::load(&root.join("build.toml"))
        .map_err(|e| LehuaError::msg(format!("could not load build.toml: {e}")))?;
    let luaurc = load_luaurc(&root)?;

    let report = bundle::build_bundle(&root, &manifest, &luaurc)?;
    for note in &report.notes {
        eprintln!("lehua: note: {note}");
    }

    let payload = bundle::serialize_payload(&report.bundle)?;
    let runtime_exe = match &runtime {
        Some(r) => {
            let p = absolutize(Path::new(r));
            if !p.is_file() {
                return Err(LehuaError::msg(format!(
                    "runtime binary '{}' not found",
                    p.display()
                )));
            }
            p
        }
        None => std::env::current_exe()?,
    };
    let target_suffix = match &runtime {
        Some(_) => runtime_exe
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default(),
        None => std::env::consts::EXE_SUFFIX.to_string(),
    };
    let out_exe = match out {
        Some(o) => absolutize(Path::new(&o)),
        None => root
            .join(&manifest.build.output)
            .join(format!("{}{}", manifest.project.name, target_suffix)),
    };

    bundle::write_executable(&runtime_exe, &payload, &out_exe)?;

    let size = std::fs::metadata(&out_exe).map(|m| m.len()).unwrap_or(0);
    println!(
        "lehua: built {} ({} modules, {} DLLs embedded, {} KB)",
        out_exe.display(),
        report.module_count,
        report.dll_count,
        size / 1024
    );
    Ok(())
}

fn cmd_clean() -> Result<()> {
    let root = absolutize(Path::new("."));
    let manifest_path = root.join("build.toml");
    let output = if manifest_path.is_file() {
        BuildManifest::load(&manifest_path)
            .map_err(|e| LehuaError::msg(format!("could not load build.toml: {e}")))?
            .build
            .output
    } else {
        "dist".to_string()
    };

    let mut targets = vec![root.join(".lehua")];
    match safe_subdir(&root, &output) {
        Some(dir) => targets.insert(0, dir),
        None => {
            return Err(LehuaError::msg(format!(
                "refusing to clean output '{output}': it must be a directory inside the project"
            )))
        }
    }

    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for dir in targets {
        if dir.exists() {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => removed.push(dir.display().to_string()),
                Err(e) => failed.push(format!("{} ({e})", dir.display())),
            }
        }
    }
    if removed.is_empty() && failed.is_empty() {
        println!("lehua: nothing to clean");
    } else if !removed.is_empty() {
        println!("lehua: cleaned {}", removed.join(", "));
    }
    if !failed.is_empty() {
        return Err(LehuaError::msg(format!(
            "could not remove {} — a file may be in use",
            failed.join(", ")
        )));
    }
    Ok(())
}

fn execute_bundle(
    bundle: Bundle,
    base_dir: PathBuf,
    extract_dir: PathBuf,
    args: Vec<String>,
    flat_dirs: bool,
) -> Result<()> {
    let dlls = Arc::new(bundle.dll_bytes());
    let files = Arc::new(bundle.files);
    let provider = Arc::new(BundleProvider::new(
        base_dir,
        files,
        dlls,
        extract_dir,
        !flat_dirs,
    ));
    let included: HashSet<String> = bundle.manifest.includes.iter().cloned().collect();
    let resolver = Arc::new(Resolver::build(
        &bundle.manifest.aliases,
        bundle.manifest.roots_dir.clone(),
        included,
    ));
    let engine = Arc::new(Engine {
        provider,
        resolver,
        entry_id: bundle.manifest.entry.clone(),
        flat_dirs,
        args: args.clone(),
    });

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        let (lua, ctx) = engine::make_vm(engine.clone())?;
        let arg_values: Vec<PortableValue> = args
            .into_iter()
            .map(|s| PortableValue::Str(s.into_bytes()))
            .collect();
        engine::run_entry(lua, ctx, &engine.entry_id, None, arg_values).await?;
        Ok::<(), LehuaError>(())
    })
}

fn load_project(path: Option<String>) -> Result<(PathBuf, BuildManifest, LuauRc)> {
    match path {
        Some(p) if p.ends_with(".luau") || Path::new(&p).is_file() => {
            let file = absolutize(Path::new(&p));
            let root = file
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let entry = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .ok_or_else(|| LehuaError::msg("invalid file path"))?;
            let manifest = BuildManifest {
                project: crate::manifest::Project {
                    name: file
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "app".to_string()),
                    version: "0.1.0".to_string(),
                    entry,
                },
                build: Default::default(),
                roots: Default::default(),
            };
            let luaurc = load_luaurc(&root)?;
            Ok((root, manifest, luaurc))
        }
        Some(dir) => {
            let root = absolutize(Path::new(&dir));
            let manifest = BuildManifest::load(&root.join("build.toml")).map_err(|e| {
                LehuaError::msg(format!("could not load {}/build.toml: {e}", root.display()))
            })?;
            let luaurc = load_luaurc(&root)?;
            Ok((root, manifest, luaurc))
        }
        None => {
            let root = absolutize(Path::new("."));
            let manifest = BuildManifest::load(&root.join("build.toml")).map_err(|e| {
                LehuaError::msg(format!(
                    "no build.toml here and no file given ({e}); try `lehua run path/to/file.luau`"
                ))
            })?;
            let luaurc = load_luaurc(&root)?;
            Ok((root, manifest, luaurc))
        }
    }
}

fn load_luaurc(root: &Path) -> Result<LuauRc> {
    let p = root.join(".luaurc");
    if p.is_file() {
        LuauRc::load(&p).map_err(|e| LehuaError::msg(format!("could not load .luaurc: {e}")))
    } else {
        Ok(LuauRc::default())
    }
}

fn absolutize(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn safe_subdir(root: &Path, rel: &str) -> Option<PathBuf> {
    let trimmed = rel.trim();
    if trimmed.is_empty() || Path::new(trimmed).is_absolute() {
        return None;
    }
    let candidate = crate::libs::normalize(&root.join(trimmed));
    if candidate.starts_with(root) && candidate != root {
        Some(candidate)
    } else {
        None
    }
}
