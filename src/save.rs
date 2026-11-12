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
