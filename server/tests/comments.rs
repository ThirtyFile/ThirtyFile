//! Comments that name a source file (`folders.rs`, `backups/policy.rs`) name one that exists: files that were moved
//! or split into folders leave comments pointing nowhere

use std::path::{Path, PathBuf};

/// The `.rs` files of the server (src/, tests/, build.rs), and Cargo.toml
fn sources(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.join("Cargo.toml"), root.join("build.rs")];
    let mut dirs = vec![root.join("src"), root.join("tests")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out
}

/// The comment of a line: after the first `//` outside a string in Rust (an even number of `"` before it; good enough
/// for this code), after `#` in Cargo.toml
fn comment(line: &str, toml: bool) -> Option<&str> {
    if toml {
        return line.find('#').map(|i| &line[i + 1..]);
    }
    line.match_indices("//").map(|(i, _)| i).find(|&i| line[..i].matches('"').count().is_multiple_of(2)).map(|i| &line[i + 2..])
}

/// The file names in a comment: words ending in `.rs`, with the folders before them (`locations/health.rs`)
fn named_files(text: &str) -> Vec<&str> {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '/';
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(".rs") {
        let after = rest[i + 3..].chars().next();
        if !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            let start = rest[..i].rfind(|c: char| !word(c)).map_or(0, |s| s + 1);
            let name = rest[start..i + 3].trim_start_matches('/');
            if name.len() > 3 {
                out.push(name);
            }
        }
        rest = &rest[i + 3..];
    }
    out
}

#[test]
fn every_file_a_comment_names_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut missing = Vec::new();
    for file in sources(root) {
        let toml = file.extension().is_some_and(|e| e == "toml");
        let text = std::fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let Some(comment) = comment(line, toml) else { continue };
            for name in named_files(comment) {
                // A path from src/, from the folder of the file, or from the server's folder (build.rs, tests/…)
                let found = [root.join("src"), file.parent().unwrap().to_path_buf(), root.to_path_buf()].iter().any(|base| base.join(name).is_file());
                if !found {
                    missing.push(format!("{}:{}: {name}", file.strip_prefix(root).unwrap().display(), n + 1));
                }
            }
        }
    }
    assert!(missing.is_empty(), "comments name files that don't exist (a file moved or became a folder?):\n{}", missing.join("\n"));
}

#[test]
fn file_names_are_found_in_comments() {
    assert_eq!(named_files(" moves (moves/mod.rs), folders.rs and the build script"), ["moves/mod.rs", "folders.rs"]);
    assert_eq!(named_files(" `locations/health.rs`: rs files, foo.rsx, .rs"), ["locations/health.rs"]);
    assert_eq!(comment("let x = 1; // see db.rs", false), Some(" see db.rs"));
    assert_eq!(comment("let url = \"https://example.com\"; // see db.rs", false), Some(" see db.rs"));
    assert_eq!(comment("let url = \"https://example.com\";", false), None);
    assert_eq!(comment("quick-xml = \"0.42\" # dav.rs", true), Some(" dav.rs"));
}
