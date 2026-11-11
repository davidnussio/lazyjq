//! Query history (History panel), optionally persisted across sessions.

use std::path::PathBuf;

pub struct History {
    /// Most recent first.
    entries: Vec<String>,
    max_entries: usize,
    file: Option<PathBuf>,
    /// Position while browsing with Up/Down in the query editor.
    browse: Option<usize>,
    pub selected: usize,
}

impl History {
    /// Load the history from `file` (if any). A missing or unreadable file means an empty history.
    pub fn load(file: Option<PathBuf>, max_entries: usize) -> Self {
        let entries = file
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|content| {
                content
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .take(max_entries)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            entries,
            max_entries,
            file,
            browse: None,
            selected: 0,
        }
    }

    pub fn default_file() -> Option<PathBuf> {
        dirs::data_dir().map(|dir| dir.join("lazyjq").join("history"))
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Add `query` on top, removing older duplicates.
    pub fn push(&mut self, query: &str) {
        let query = query.trim();
        // Queries are stored one per line.
        if query.is_empty() || query == "." || query.contains('\n') {
            return;
        }
        self.entries.retain(|entry| entry != query);
        self.entries.insert(0, query.to_string());
        self.entries.truncate(self.max_entries);
        self.selected = 0;
        self.browse = None;
    }

    pub fn remove_selected(&mut self) {
        if self.selected < self.entries.len() {
            self.entries.remove(self.selected);
            self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        }
    }

    /// Step back in time (Up in the query editor).
    pub fn older(&mut self) -> Option<&str> {
        let next = self.browse.map_or(0, |i| i + 1);
        if next < self.entries.len() {
            self.browse = Some(next);
        }
        self.browse.map(|i| self.entries[i].as_str())
    }

    /// Step forward in time (Down in the query editor). `None` past the newest entry.
    pub fn newer(&mut self) -> Option<&str> {
        match self.browse {
            Some(0) | None => {
                self.browse = None;
                None
            }
            Some(i) => {
                self.browse = Some(i - 1);
                Some(self.entries[i - 1].as_str())
            }
        }
    }

    pub fn reset_browse(&mut self) {
        self.browse = None;
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.file else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut content = self.entries.join("\n");
        content.push('\n');
        std::fs::write(path, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_dedupes_and_limits() {
        let mut h = History::load(None, 2);
        h.push(".a");
        h.push(".b");
        h.push(".a");
        h.push(".");
        assert_eq!(h.entries(), [".a", ".b"]);
        h.push(".c");
        assert_eq!(h.entries(), [".c", ".a"]);
    }

    #[test]
    fn browsing() {
        let mut h = History::load(None, 10);
        h.push(".a");
        h.push(".b");
        assert_eq!(h.older(), Some(".b"));
        assert_eq!(h.older(), Some(".a"));
        assert_eq!(h.older(), Some(".a"));
        assert_eq!(h.newer(), Some(".b"));
        assert_eq!(h.newer(), None);
    }

    #[test]
    fn persists() {
        let dir = std::env::temp_dir().join(format!("lazyjq-history-{}", std::process::id()));
        let file = dir.join("history");
        let mut h = History::load(Some(file.clone()), 10);
        h.push(".x");
        h.save().unwrap();
        assert_eq!(History::load(Some(file), 10).entries(), [".x"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
