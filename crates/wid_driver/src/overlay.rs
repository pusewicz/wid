//! Unsaved editor buffers that the loader reads instead of the files on
//! disk, for `wid lsp`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::loader::clean_path;

/// The text of files as an editor holds them, by path. The loader reads a
/// file's text from here when it is present and from disk otherwise, and a
/// `.wid` file here belongs to its directory's package even when it isn't
/// on disk yet. Paths are made absolute and cleaned (`a/./b/../c` is
/// `a/c`) without touching the file system, so a key matches however the
/// loader spells the path; symbolic links aren't followed.
///
/// An empty overlay, which the command-line tools pass, changes nothing.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    files: BTreeMap<PathBuf, Arc<str>>,
}

impl Overlay {
    /// An empty overlay.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the text of the file at `path`.
    pub fn insert(&mut self, path: &Path, text: impl Into<Arc<str>>) {
        self.files.insert(normalize(path), text.into());
    }

    /// Forgets the file at `path`, so the loader reads it from disk again.
    pub fn remove(&mut self, path: &Path) {
        self.files.remove(&normalize(path));
    }

    /// The text of the file at `path`, if the overlay holds it.
    pub fn get(&self, path: &Path) -> Option<&Arc<str>> {
        if self.files.is_empty() {
            return None;
        }
        self.files.get(&normalize(path))
    }

    /// Whether the overlay holds no file.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The files the overlay holds directly in `dir`, by their normalized
    /// paths.
    pub(crate) fn files_in(&self, dir: &Path) -> impl Iterator<Item = &PathBuf> {
        let dir = (!self.files.is_empty()).then(|| normalize(dir));
        self.files.keys().filter(move |p| dir.as_deref().is_some_and(|d| p.parent() == Some(d)))
    }
}

/// `path` made absolute against the current directory and cleaned.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    clean_path(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::Overlay;

    #[test]
    fn paths_match_however_they_are_spelled() {
        let root = std::env::temp_dir().join("wid-overlay");
        let mut overlay = Overlay::new();
        overlay.insert(&root.join("pkg/./main.wid"), "def main\nend\n");
        assert_eq!(overlay.get(&root.join("pkg/main.wid")).map(|t| t.as_ref()), Some("def main\nend\n"));
        assert_eq!(overlay.get(&root.join("pkg/sub/../main.wid")).map(|t| t.len()), Some(13));
        let listed: Vec<&PathBuf> = overlay.files_in(&root.join("pkg")).collect();
        assert_eq!(listed, [&root.join("pkg/main.wid")]);
        assert_eq!(overlay.files_in(&root).count(), 0);
        overlay.remove(&root.join("pkg/main.wid"));
        assert!(overlay.get(&root.join("pkg/main.wid")).is_none());
        assert!(overlay.is_empty());
        assert_eq!(overlay.files_in(Path::new(".")).count(), 0);
    }

    /// The checker reads an unsaved buffer instead of the file on disk, and
    /// a buffer not on disk yet belongs to its directory's package.
    #[test]
    fn analyze_reads_the_overlay() {
        let dir = std::env::temp_dir().join(format!("wid-overlay-analyze-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the package");
        std::fs::write(dir.join("main.wid"), "def main\n  puts 1\nend\n").expect("write main.wid");
        let opts = crate::Options::new(&dir);
        let codes = |overlay: &Overlay| -> Vec<&'static str> {
            crate::analyze(&opts, overlay).diags.iter().map(|d| d.code.as_str()).collect()
        };
        assert_eq!(codes(&Overlay::new()), Vec::<&str>::new());
        let mut overlay = Overlay::new();
        overlay.insert(&dir.join("main.wid"), "def main\n  puts missing\nend\n");
        assert_eq!(codes(&overlay), ["E0201"]);
        overlay.insert(&dir.join("main.wid"), "def main\n  puts helper\nend\n");
        assert_eq!(codes(&overlay), ["E0201"], "helper isn't declared yet");
        overlay.insert(&dir.join("unsaved.wid"), "def helper -> Int = 2\n");
        assert_eq!(codes(&overlay), Vec::<&str>::new(), "the unsaved file is part of the package");
        let mut file_opts = crate::Options::new(dir.join("scratch.wid"));
        file_opts.file_mode = true;
        overlay.insert(&dir.join("scratch.wid"), "def main\n  puts nope\nend\n");
        let analysis = crate::analyze(&file_opts, &overlay);
        let found: Vec<&str> = analysis.diags.iter().map(|d| d.code.as_str()).collect();
        assert_eq!(found, ["E0201"], "a `-file` target only the overlay holds");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
