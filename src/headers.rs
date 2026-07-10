#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Directives {
    pub includes: Vec<String>,
    pub injects: Vec<String>,
}
const RESERVED: &[&str] = &["require", "parallel", "__dirname", "__filename", "messenger"];

const LUAU_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while", "continue", "export",
    "type",
];

pub fn inject_global_name(entry: &str) -> String {
    let entry = entry.replace('\\', "/");
    let file = entry.rsplit('/').next().unwrap_or(&entry);
    let stem = match file.rfind('.') {
        Some(dot) if dot > 0 => &file[..dot],
        _ => file,
    };
    let mut out = String::with_capacity(stem.len() + 1);
    for (i, c) in stem.chars().enumerate() {
        if c.is_ascii_alphanumeric() || c == '_' {
            if i == 0 && c.is_ascii_digit() {
                out.push('_');
            }
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    if RESERVED.contains(&out.as_str()) || LUAU_KEYWORDS.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

pub fn parse(source: &str) -> Directives {
    let mut d = Directives::default();
    let mut in_block: Option<usize> = None;
    for line in source.lines() {
        if let Some(level) = in_block {
            if find_long_close(line, level).is_some() {
                in_block = None;
            }
            continue;
        }
        let t = line.trim_start();
        if t.is_empty() || t.starts_with("#!") {
            continue;
        }
        if let Some(rest) = t.strip_prefix("--#") {
            if let Some((name, args)) = split_directive(rest) {
                let items = args
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                match name.trim() {
                    "include" => {
                        for it in items {
                            push_unique(&mut d.includes, it);
                        }
                    }
                    "inject" => {
                        for it in items {
                            push_unique(&mut d.injects, it);
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }
        if let Some(after) = t.strip_prefix("--") {
            let a = after.trim_start();
            if let Some(level) = long_open_level(a) {
                if find_long_close(a, level).is_none() {
                    in_block = Some(level);
                }
            }
            continue;
        }
        break;
    }
    d
}

fn long_open_level(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.first() != Some(&b'[') {
        return None;
    }
    let mut j = 1;
    while b.get(j) == Some(&b'=') {
        j += 1;
    }
    if b.get(j) == Some(&b'[') {
        Some(j - 1)
    } else {
        None
    }
}

fn find_long_close(s: &str, level: usize) -> Option<usize> {
    let mut needle = String::with_capacity(level + 2);
    needle.push(']');
    for _ in 0..level {
        needle.push('=');
    }
    needle.push(']');
    s.find(&needle)
}

fn split_directive(rest: &str) -> Option<(&str, &str)> {
    let open = rest.find('[')?;
    let close = rest.rfind(']')?;
    if close <= open {
        return None;
    }
    Some((&rest[..open], &rest[open + 1..close]))
}

fn push_unique(v: &mut Vec<String>, item: String) {
    if !v.contains(&item) {
        v.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_header_directives() {
        let src = "--!strict\n--#include[fs, os]\n\n--#inject[math.dll]\nlocal x = 1\n--#include[late]";
        let d = parse(src);
        assert_eq!(d.includes, vec!["fs", "os"]);
        assert_eq!(d.injects, vec!["math.dll"]);
    }

    #[test]
    fn inject_name_is_sanitized_file_stem() {
        assert_eq!(inject_global_name("./native/MathExt.dll"), "MathExt");
        assert_eq!(inject_global_name("plain"), "plain");
        assert_eq!(inject_global_name("my-lib.dll"), "my_lib");
        assert_eq!(inject_global_name("2fast.dll"), "_2fast");
    }
}
