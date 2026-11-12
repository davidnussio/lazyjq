//! Writing the result to a file.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

/// The file proposed when saving for the first time: next to the input file
/// (`data.json` → `data.result.json`), or `result.json` when reading stdin.
pub fn default_path(source: Option<&Path>) -> String {
    let Some(source) = source else {
        return "result.json".into();
    };
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "result".into());
    let name = match source.extension() {
        Some(ext) => format!("{stem}.result.{}", ext.to_string_lossy()),
        None => format!("{stem}.result.json"),
    };
    source.with_file_name(name).display().to_string()
}

/// The path typed by the user, with a leading `~` expanded to the home directory.
pub fn resolve_path(text: &str) -> PathBuf {
    expand_tilde(text, dirs::home_dir())
}

fn expand_tilde(text: &str, home: Option<PathBuf>) -> PathBuf {
    let rest = match text.strip_prefix('~') {
        Some("") => "",
        Some(rest) if rest.starts_with('/') || rest.starts_with(std::path::MAIN_SEPARATOR) => {
            &rest[1..]
        }
        _ => return PathBuf::from(text),
    };
    match home {
        Some(home) => home.join(rest),
        None => PathBuf::from(text),
    }
}

/// Completions of the file name being typed: the text before its last
/// component, and the names in that directory starting with that component
/// (sorted, directories ending with `/`). Hidden files are only listed when
/// the component starts with `.`; when nothing matches, case is ignored.
pub fn complete_path(text: &str) -> (String, Vec<String>) {
    complete_path_in(text, dirs::home_dir())
}

fn complete_path_in(text: &str, home: Option<PathBuf>) -> (String, Vec<String>) {
    if text == "~" {
        return (String::new(), vec!["~/".into()]);
    }
    let split = text.rfind('/').map_or(0, |i| i + 1);
    let (base, prefix) = text.split_at(split);
    let dir = if base.is_empty() {
        PathBuf::from(".")
    } else {
        expand_tilde(base, home)
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return (base.to_string(), Vec::new());
    };
    let entries: Vec<(String, bool)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if name.starts_with('.') && !prefix.starts_with('.') {
                return None;
            }
            // Follows symbolic links, so that a link to a directory completes like one.
            let is_dir = entry.path().is_dir();
            Some((name, is_dir))
        })
        .collect();

    let matching = |ignore_case: bool| -> Vec<String> {
        let lower = prefix.to_lowercase();
        let mut names: Vec<String> = entries
            .iter()
            .filter(|(name, _)| {
                if ignore_case {
                    name.to_lowercase().starts_with(&lower)
                } else {
                    name.starts_with(prefix)
                }
            })
            .map(|(name, is_dir)| {
                if *is_dir {
                    format!("{name}/")
                } else {
                    name.clone()
                }
            })
            .collect();
        names.sort();
        names
    };
    let mut names = matching(false);
    if names.is_empty() {
        names = matching(true);
    }
    (base.to_string(), names)
}

/// The longest common beginning of `names`.
pub fn common_prefix(names: &[String]) -> String {
    let Some((first, rest)) = names.split_first() else {
        return String::new();
    };
    let mut len = first.len();
    for name in rest {
        len = first
            .char_indices()
            .zip(name.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((i, a), _)| i + a.len_utf8())
            .min(len);
    }
    first[..len].to_string()
}

/// Write `content` to `path` without ever leaving a half written file behind:
/// the content goes to a temporary file in the same directory, which then
/// replaces the target. An existing file keeps its permissions, and a symbolic
/// link keeps pointing to the file it points to (which gets the new content).
pub fn write_atomic(path: &Path, content: &[u8]) -> io::Result<()> {
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(e) if e.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(e),
    };
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file name"))?;
    let temp = target.with_file_name(format!(
        ".{}.lazyjq-{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));

    let write = || -> io::Result<()> {
        let mut file = fs::File::create(&temp)?;
        file.write_all(content)?;
        file.sync_all()?;
        if let Ok(metadata) = fs::metadata(&target) {
            fs::set_permissions(&temp, metadata.permissions())?;
        }
        fs::rename(&temp, &target)
    };
    write().inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

/// A byte count for humans: `512 B`, `1.5 KB`, `12.0 MB`.
pub fn human_size(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lazyjq-save-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_path_is_next_to_the_input() {
        assert_eq!(default_path(None), "result.json");
        assert_eq!(
            default_path(Some(Path::new("data/users.json"))),
            "data/users.result.json"
        );
        assert_eq!(
            default_path(Some(Path::new("events.jsonl"))),
            "events.result.jsonl"
        );
        assert_eq!(default_path(Some(Path::new("dump"))), "dump.result.json");
    }

    #[test]
    fn tilde_is_expanded() {
        let home = Some(PathBuf::from("/home/me"));
        assert_eq!(
            expand_tilde("~/out.json", home.clone()),
            PathBuf::from("/home/me/out.json")
        );
        assert_eq!(expand_tilde("~", home.clone()), PathBuf::from("/home/me"));
        assert_eq!(
            expand_tilde("~other/x", home.clone()),
            PathBuf::from("~other/x")
        );
        assert_eq!(expand_tilde("out.json", home), PathBuf::from("out.json"));
        assert_eq!(expand_tilde("~/x", None), PathBuf::from("~/x"));
    }

    #[test]
    fn completes_file_names() {
        let dir = temp_dir("complete");
        fs::write(dir.join("alpha.json"), "").unwrap();
        fs::write(dir.join("alps.txt"), "").unwrap();
        fs::write(dir.join(".hidden"), "").unwrap();
        fs::create_dir(dir.join("beta")).unwrap();
        let base = format!("{}/", dir.display());
        let complete = |typed: &str| complete_path_in(&format!("{base}{typed}"), None);

        assert_eq!(
            complete("al"),
            (base.clone(), vec!["alpha.json".into(), "alps.txt".into()])
        );
        assert_eq!(complete("b").1, ["beta/"]);
        assert_eq!(complete("").1, ["alpha.json", "alps.txt", "beta/"]);
        assert_eq!(complete(".").1, [".hidden"]);
        assert_eq!(complete("AL").1, ["alpha.json", "alps.txt"]);
        assert!(complete("zzz").1.is_empty());
        assert!(complete_path_in("/nonexistent-lazyjq/x", None).1.is_empty());

        let home = Some(dir.clone());
        assert_eq!(
            complete_path_in("~/be", home.clone()),
            ("~/".into(), vec!["beta/".into()])
        );
        assert_eq!(complete_path_in("~", home).1, ["~/"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn common_prefixes() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(common_prefix(&names(&["alpha.json", "alps.txt"])), "alp");
        assert_eq!(common_prefix(&names(&["àbc", "àbd"])), "àb");
        assert_eq!(common_prefix(&names(&["a", "b"])), "");
        assert_eq!(common_prefix(&names(&["same"])), "same");
        assert_eq!(common_prefix(&[]), "");
    }

    #[test]
    fn writes_and_overwrites() {
        let dir = temp_dir("write");
        let file = dir.join("out.json");
        write_atomic(&file, b"1\n").unwrap();
        write_atomic(&file, b"2\n").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "2\n");
        // No temporary file is left behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_directory_is_an_error() {
        let dir = temp_dir("missing");
        assert!(write_atomic(&dir.join("nope").join("out.json"), b"1").is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn keeps_permissions_and_symbolic_links() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("unix");
        let real = dir.join("real.json");
        let link = dir.join("link.json");
        fs::write(&real, "old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomic(&link, b"new").unwrap();
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(12 * 1024 * 1024), "12.0 MB");
    }
}
