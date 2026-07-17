pub fn normalize(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(&last) if last != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

pub fn join(base_dir: &str, rel: &str) -> String {
    if base_dir.is_empty() {
        normalize(rel)
    } else {
        normalize(&format!("{base_dir}/{rel}"))
    }
}

pub fn to_native(vpath: &str) -> String {
    if std::path::MAIN_SEPARATOR == '\\' {
        vpath.replace('/', "\\")
    } else {
        vpath.to_string()
    }
}

pub fn dirname(id: &str) -> String {
    match id.rfind('/') {
        Some(i) => id[..i].to_string(),
        None => String::new(),
    }
}

pub fn resolve_include(module_id: &str, spec: &str) -> String {
    let spec = spec.trim();
    let rel = match spec.strip_prefix("@self") {
        Some(rest) => {
            let rest = rest.trim_start_matches('/');
            if rest.is_empty() {
                String::from(".")
            } else {
                rest.to_string()
            }
        }
        None => spec.to_string(),
    };
    join(&dirname(module_id), &rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize("./src/../src/main.luau"), "src/main.luau");
        assert_eq!(normalize("a/b/../c"), "a/c");
        assert_eq!(normalize("../a"), "../a");
        assert_eq!(normalize("a/./b"), "a/b");
    }

    #[test]
    fn joins_and_dirnames() {
        assert_eq!(join("src", "./util"), "src/util");
        assert_eq!(join("src/a", "../b"), "src/b");
        assert_eq!(dirname("src/a/b.luau"), "src/a");
        assert_eq!(dirname("main.luau"), "");
    }
}
