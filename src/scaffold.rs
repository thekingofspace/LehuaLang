use std::path::Path;

use crate::error::Result;

const BUILD_TOML: &str = include_str!("templates/build.toml");
const LUAURC: &str = include_str!("templates/luaurc.json");
const GITIGNORE: &str = include_str!("templates/gitignore");
const TYPES_D: &str = include_str!("templates/types.d.luau");
const MAIN_LUAU: &str = include_str!("templates/main.luau");
const WORKER_LUAU: &str = include_str!("templates/worker.luau");
const HELLO_LIB_TOML: &str = include_str!("templates/hello.lib.toml");
const HELLO_INIT_LUAU: &str = include_str!("templates/hello.init.luau");
const VSCODE_SETTINGS: &str = include_str!("templates/vscode-settings.json");

pub fn init(name: Option<String>) -> Result<()> {
    let dir = name.clone().unwrap_or_else(|| ".".to_string());
    let root = Path::new(&dir);
    let project_name = match &name {
        Some(n) if n != "." => Path::new(n)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "app".to_string()),
        _ => std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "app".to_string()),
    };

    write_if_absent(&root.join("build.toml"), &BUILD_TOML.replace("{name}", &project_name))?;
    write_if_absent(&root.join(".luaurc"), LUAURC)?;
    write_if_absent(&root.join(".gitignore"), GITIGNORE)?;
    write_always(&root.join("types.d.luau"), TYPES_D)?;
    write_if_absent(&root.join("src/main.luau"), MAIN_LUAU)?;
    write_if_absent(&root.join("src/worker.luau"), WORKER_LUAU)?;
    write_if_absent(&root.join("roots/hello/lib.toml"), HELLO_LIB_TOML)?;
    write_if_absent(&root.join("roots/hello/init.luau"), HELLO_INIT_LUAU)?;
    write_if_absent(&root.join(".vscode/settings.json"), VSCODE_SETTINGS)?;

    println!(
        "lehua: initialized project '{project_name}'\n  run:   lehua run\n  build: lehua build"
    );
    Ok(())
}

fn write_if_absent(path: &Path, contents: &str) -> Result<()> {
    if path.exists() {
        println!("lehua: skipped existing {}", path.display());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    println!("lehua: created {}", path.display());
    Ok(())
}

fn write_always(path: &Path, contents: &str) -> Result<()> {
    let existed = path.exists();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    println!("lehua: {} {}", if existed { "updated" } else { "created" }, path.display());
    Ok(())
}
