#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Directives {
    pub includes: Vec<String>,
    pub injects: Vec<String>,
    pub include_strings: Vec<(String, String)>,
}
const RESERVED: &[&str] = &[
    "require",
    "parallel",
    "frominclude",
    "__dirname",
    "__filename",
    "messenger",
];

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
                    "includestring" => {
                        for (key, spec) in parse_include_strings(args) {
                            if !d.include_strings.iter().any(|(k, _)| *k == key) {
                                d.include_strings.push((key, spec));
                            }
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

fn parse_include_strings(args: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in split_top_level(args) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (key, spec) = match top_level_colon(entry) {
            Some(i) => (unquote(entry[..i].trim()), unquote(entry[i + 1..].trim())),
            None => {
                let spec = unquote(entry);
                (spec.clone(), spec)
            }
        };
        if !key.is_empty() && !spec.is_empty() {
            out.push((key, spec));
        }
    }
    out
}

fn top_level_colon(s: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                ':' => return Some(i),
                _ => {}
            },
        }
    }
    None
}

fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                ',' => out.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            },
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
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
    fn parses_include_strings() {
        let src = "--#includestring[Config: \"./config.json\", Readme: \"@self/README.md\"]\nlocal x = 1";
        let d = parse(src);
        assert_eq!(
            d.include_strings,
            vec![
                ("Config".to_string(), "./config.json".to_string()),
                ("Readme".to_string(), "@self/README.md".to_string()),
            ]
        );
    }

    #[test]
    fn include_string_key_defaults_to_path() {
        let d = parse("--#includestring[\"./data.txt\"]");
        assert_eq!(
            d.include_strings,
            vec![("./data.txt".to_string(), "./data.txt".to_string())]
        );
    }

    #[test]
    fn include_string_ignores_colons_inside_quotes() {
        let d = parse("--#includestring[\"./odd:name.txt\", Log: './a:b.txt']");
        assert_eq!(
            d.include_strings,
            vec![
                ("./odd:name.txt".to_string(), "./odd:name.txt".to_string()),
                ("Log".to_string(), "./a:b.txt".to_string()),
            ]
        );
    }

    #[test]
    fn include_string_dedupes_first_key_wins() {
        let d = parse("--#includestring[A: \"./x\"]\n--#includestring[A: \"./y\", B: \"./z\"]");
        assert_eq!(
            d.include_strings,
            vec![
                ("A".to_string(), "./x".to_string()),
                ("B".to_string(), "./z".to_string()),
            ]
        );
    }

    #[test]
    fn inject_name_is_sanitized_file_stem() {
        assert_eq!(inject_global_name("./native/MathExt.dll"), "MathExt");
        assert_eq!(inject_global_name("plain"), "plain");
        assert_eq!(inject_global_name("my-lib.dll"), "my_lib");
        assert_eq!(inject_global_name("2fast.dll"), "_2fast");
    }
}
