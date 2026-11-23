//! What files the commands running may read and write ([`FileAccess`]), and
//! the scope a plugin command or event handler runs in ([`PluginScope`]).
//!
//! A [`Session`](crate::Session) keeps the access of the commands running:
//! everything for the user's own commands; nothing inside a graph run
//! without `allow_files` (see [`Session::with_files`](crate::Session::with_files));
//! and, inside a plugin command whose plugin was not granted the `files`
//! permission, only the paths its arguments grant ([`Grants`]), the ones the
//! user named in its `save_path` (write) and `open_path` (read) parameters.
//! Every command that reads or writes a path checks it with
//! [`Session::require_read`](crate::Session::require_read) or
//! [`Session::require_write`](crate::Session::require_write).
//!
//! Paths are compared by a key: the canonical path of a file to read (it
//! must exist), and the canonical folder plus the file name of a file to
//! write (the canonical path when it exists), so `a/../b.idf`, `./b.idf` and
//! an absolute spelling are the same file.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// Paths a run may read and paths it may write, by their keys
/// ([`read_key`], [`write_key`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    read: BTreeSet<PathBuf>,
    read_dirs: BTreeSet<PathBuf>,
    write: BTreeSet<PathBuf>,
}

impl Grants {
    pub fn new() -> Grants {
        Grants::default()
    }

    /// Grant reading `path` (an existing file); returns its key.
    pub fn grant_read(&mut self, path: &Path) -> Result<PathBuf, String> {
        let key = read_key(path)?;
        self.read.insert(key.clone());
        Ok(key)
    }

    /// Grant reading any existing file inside `dir` (for example, a plugin's
    /// own isolated download cache).
    pub fn grant_read_dir(&mut self, dir: &Path) -> Result<PathBuf, String> {
        let key = std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        self.read_dirs.insert(key.clone());
        Ok(key)
    }

    /// Grant writing `path` (a file in an existing folder); returns its key.
    pub fn grant_write(&mut self, path: &Path) -> Result<PathBuf, String> {
        let key = write_key(path)?;
        self.write.insert(key.clone());
        Ok(key)
    }

    /// Whether `path` may be read.
    pub fn may_read(&self, path: &Path) -> bool {
        read_key(path).is_ok_and(|k| self.read.contains(&k) || self.read_dirs.iter().any(|d| k.starts_with(d)))
    }

    /// Whether `path` may be written.
    pub fn may_write(&self, path: &Path) -> bool {
        write_key(path).is_ok_and(|k| self.write.contains(&k))
    }

    /// The keys of the paths that may be read.
    pub fn reads(&self) -> impl Iterator<Item = &Path> {
        self.read.iter().map(PathBuf::as_path)
    }

    /// The keys of the paths that may be written.
    pub fn writes(&self) -> impl Iterator<Item = &Path> {
        self.write.iter().map(PathBuf::as_path)
    }

    fn both(&self, other: &Grants) -> Grants {
        Grants {
            read: self.read.intersection(&other.read).cloned().collect(),
            read_dirs: self.read_dirs.intersection(&other.read_dirs).cloned().collect(),
            write: self.write.intersection(&other.write).cloned().collect(),
        }
    }
}

/// What files the commands running may read and write.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FileAccess {
    /// Any file (the user's own commands, plugins with the `files` permission).
    #[default]
    All,
    /// No file (a graph run without `allow_files`).
    None,
    /// Only these paths, each in its direction (a plugin command without the
    /// `files` permission: the paths of its `save_path` and `open_path`
    /// arguments).
    Granted(Grants),
}

impl FileAccess {
    /// Whether any file may be read and written.
    pub fn is_all(&self) -> bool {
        matches!(self, FileAccess::All)
    }

    /// What both allow (access is only ever narrowed).
    pub fn narrow(&self, other: &FileAccess) -> FileAccess {
        match (self, other) {
            (FileAccess::All, x) | (x, FileAccess::All) => x.clone(),
            (FileAccess::None, _) | (_, FileAccess::None) => FileAccess::None,
            (FileAccess::Granted(a), FileAccess::Granted(b)) => FileAccess::Granted(a.both(b)),
        }
    }

    /// Whether `path` may be read.
    pub fn may_read(&self, path: &Path) -> bool {
        match self {
            FileAccess::All => true,
            FileAccess::None => false,
            FileAccess::Granted(g) => g.may_read(path),
        }
    }

    /// Whether `path` may be written.
    pub fn may_write(&self, path: &Path) -> bool {
        match self {
            FileAccess::All => true,
            FileAccess::None => false,
            FileAccess::Granted(g) => g.may_write(path),
        }
    }
}

/// What a plugin command (or event handler) may do while it runs: every
/// command run during it (also by graphs, scripts or macros it starts) is
/// checked against this by [`Session::execute`](crate::Session::execute).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginScope {
    /// The plugin's id: its `plugin_data_*` calls must name it.
    pub plugin: String,
    /// Which commands that change the model may run.
    pub edits: ScopeEdits,
    /// The files it may read and write.
    pub files: FileAccess,
}

/// Which commands that change the model a [`PluginScope`] lets run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeEdits {
    /// None (a query command).
    None,
    /// Only `plugin_data_set` for the scope's own plugin (an event handler
    /// keeping its data up to date).
    OwnData,
    /// Any (an edit or file command).
    All,
}

/// `path` made absolute (from the current folder) with `.` and `..` resolved
/// by name.
pub fn lexical(path: &Path) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out)
}

/// The key of a file to read: its canonical path (it must be a file).
pub fn read_key(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("an empty path".into());
    }
    let shown = path.display();
    let real = lexical(path)?.canonicalize().map_err(|e| format!("{shown}: {e}"))?;
    if !real.is_file() {
        return Err(format!("{shown} is not a file"));
    }
    Ok(real)
}

/// The key of a file to write: its canonical folder plus its name (its
/// canonical path when it exists, which must not be a folder).
pub fn write_key(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("an empty path".into());
    }
    let shown = path.display();
    let full = lexical(path)?;
    if let Ok(real) = full.canonicalize() {
        if real.is_dir() {
            return Err(format!("{shown} is a folder"));
        }
        return Ok(real);
    }
    let name = full.file_name().ok_or_else(|| format!("{shown}: no file name"))?;
    let parent = full.parent().ok_or_else(|| format!("{shown}: no folder"))?;
    let folder = parent.canonicalize().map_err(|e| format!("{}: {e}", parent.display()))?;
    Ok(folder.join(name))
}

/// The extension of `path`, lowercase (empty when it has none).
pub fn extension_of(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_spellings_of_one_file() {
        let dir = std::env::temp_dir().join(format!("cc-files-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let other = dir.join("sub").join("..").join(".").join("a.txt");
        assert_eq!(read_key(&file).unwrap(), read_key(&other).unwrap());
        let new = dir.join("sub").join("..").join("new.idf");
        assert_eq!(write_key(&new).unwrap(), write_key(&dir.join("new.idf")).unwrap());
        assert!(read_key(&dir.join("missing.txt")).is_err());
        assert!(write_key(&dir.join("missing").join("x.idf")).is_err());
        assert!(write_key(&dir.join("sub")).is_err());
        assert!(write_key(Path::new("")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn access_only_narrows() {
        let dir = std::env::temp_dir().join(format!("cc-files-n-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut g = Grants::new();
        g.grant_write(&dir.join("out.idf")).unwrap();
        let granted = FileAccess::Granted(g);
        assert_eq!(FileAccess::All.narrow(&granted), granted);
        assert_eq!(granted.narrow(&FileAccess::None), FileAccess::None);
        assert!(granted.may_write(&dir.join("out.idf")));
        assert!(!granted.may_read(&dir.join("out.idf")));
        assert!(!granted.may_write(&dir.join("other.idf")));
        assert_eq!(granted.narrow(&FileAccess::Granted(Grants::new())), FileAccess::Granted(Grants::new()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
