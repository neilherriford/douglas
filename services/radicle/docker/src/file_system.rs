use mockall::automock;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{Metadata, create_dir_all, metadata, read_dir, read_link, remove_file, rename};
use std::io::ErrorKind;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

#[automock]
pub trait FileSystem {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, std::io::Error>;
    fn create_dir_all(&self, path: &Path) -> Result<PathBuf, std::io::Error>;
    fn delete(&self, path: &Path) -> Result<(), std::io::Error>;
    fn entries(&self, path: &Path) -> Result<Vec<Entry>, std::io::Error>;
    fn exists(&self, path: &Path) -> bool;
    fn is_directory(&self, path: &Path) -> bool;
    fn link(&self, from: &Path, to: &Path) -> Result<(), std::io::Error>;
    fn read_link(&self, path: &Path) -> Result<PathBuf, std::io::Error>;
    fn read_metadata(&self, path: &Path) -> Result<Entry, std::io::Error>;
    fn rename(&self, from: &Path, to: &Path) -> Result<PathBuf, std::io::Error>;
}

impl MockFileSystem {
    pub fn add_path_to_tracking(tracking: &Arc<Mutex<HashSet<PathBuf>>>, path: &str) {
        let local = Arc::clone(&tracking);
        let mut fs = local.lock().unwrap();
        fs.insert(Path::new(path).to_path_buf());
    }

    pub fn tracking_contains(tracking: &Arc<Mutex<HashSet<PathBuf>>>, path: &str) -> bool {
        let local = Arc::clone(&tracking);
        let fs = local.lock().unwrap();
        fs.contains(Path::new(path))
    }

    pub fn expect_exists_with_tracking(&mut self, tracking: &Arc<Mutex<HashSet<PathBuf>>>) {
        let local_tracking = Arc::clone(&tracking);
        self.expect_exists().returning(move |path: &Path| {
            let mut exists = local_tracking.lock().unwrap();
            let entry = path.to_path_buf();
            if exists.contains(&entry) {
                true
            } else {
                exists.insert(entry);
                false
            }
        });
    }

    pub fn expect_create_dir_all_with_tracking(&mut self, tracking: &Arc<Mutex<HashSet<PathBuf>>>) {
        let local_tracking = Arc::clone(&tracking);
        self.expect_create_dir_all().returning(move |path: &Path| {
            let mut created = local_tracking.lock().unwrap();

            let entry = path.to_path_buf();
            if !created.contains(&entry) {
                created.insert(entry);
            }
            Ok(path.to_path_buf())
        });
    }

    pub fn expect_path_exists(&mut self, path: &str) {
        self.expect_path(path, true);
    }
    pub fn expect_path_does_not_exist(&mut self, path: &str) {
        self.expect_path(path, false);
    }

    pub fn expect_path(&mut self, path: &str, exists: bool) {
        let expected_path = path.to_string();
        self.expect_exists()
            .withf(move |path| path == Path::new(&expected_path))
            .times(1)
            .return_once(move |_path| exists);
    }

    pub fn expect_entries_for_path(&mut self, path: &str, entries: Vec<Entry>) {
        let expected_path = path.to_string();
        self.expect_entries()
            .withf(move |path| path == Path::new(&expected_path))
            .times(1)
            .return_once(move |_path| Ok(entries));
    }

    pub fn expect_path_exists_with_entries(&mut self, path: &str, entries: Vec<Entry>) {
        self.expect_path_exists(path);
        let expected_path = path.to_string();
        self.expect_entries()
            .withf(move |path| path == Path::new(&expected_path))
            .times(1)
            .return_once(move |_path| Ok(entries));
    }

    pub fn expect_create_dir_all_for_path(&mut self, path: &str) {
        let input = path.to_string();
        let output = path.to_string();

        self.expect_create_dir_all()
            .withf(move |path| path == Path::new(&input))
            .times(1)
            .return_once(move |_path| Ok(PathBuf::from(&output)));
    }

    pub fn expect_link_to_paths(&mut self, from: &str, to: &str) {
        let expected_from = from.to_string();
        let expected_to = to.to_string();

        self.expect_link()
            .withf(move |from, to| {
                from == Path::new(&expected_from) && to == Path::new(&expected_to)
            })
            .times(1)
            .return_once(move |_from, _to| Ok(()));
    }

    pub fn expect_delete_path(&mut self, path: &str) {
        let expected_path = path.to_string();
        self.expect_delete()
            .withf(move |path| path == Path::new(&expected_path))
            .times(1)
            .return_once(move |_path| Ok(()));
    }

    pub fn expect_read_link_with_path(&mut self, path: &str, source: &str) {
        let expected_path = path.to_string();
        let source_path = source.to_string();
        self.expect_read_link()
            .withf(move |path| path == Path::new(&expected_path))
            .times(1)
            .return_once(move |_path| Ok(Path::new(&source_path).to_path_buf()));
    }

    pub fn expect_read_metadata_with_path(&mut self, path: &str, entry: Entry) {
        let expected_path = path.to_string();
        self.expect_read_metadata()
            .withf(move |path| path == Path::new(&expected_path))
            .times(1)
            .return_once(move |_path| Ok(entry));
    }
}

pub struct LocalFileSystem {}

#[derive(Debug, PartialEq, Clone)]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub is_link: bool,
}

impl FileSystem for LocalFileSystem {
    fn is_directory(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn canonicalize(&self, path: &Path) -> Result<PathBuf, std::io::Error> {
        path.canonicalize()
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn create_dir_all(&self, path: &Path) -> Result<PathBuf, std::io::Error> {
        create_dir_all(path)?;
        self.canonicalize(path)
    }

    fn delete(&self, path: &Path) -> Result<(), std::io::Error> {
        remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<PathBuf, std::io::Error> {
        rename(from, to)?;
        self.canonicalize(to)
    }

    fn link(&self, from: &Path, to: &Path) -> Result<(), std::io::Error> {
        symlink(from, to)
    }

    fn read_link(&self, path: &Path) -> Result<PathBuf, std::io::Error> {
        read_link(path)
    }

    fn entries(&self, path: &Path) -> Result<Vec<Entry>, std::io::Error> {
        let mut result = Vec::<Entry>::new();

        result.extend(read_dir(path)?.into_iter().filter_map(|entry| {
            let entry = entry.ok()?;
            Some(
                self.try_create_entry(entry.file_name(), entry.metadata())
                    .ok()?,
            )
        }));

        Ok(result)
    }

    fn read_metadata(&self, path: &Path) -> Result<Entry, std::io::Error> {
        let name = match path.file_name() {
            Some(name) => name.into(),
            None => {
                return Err(std::io::Error::new(
                    ErrorKind::Other,
                    "Missing name".to_string(),
                ));
            }
        };
        let metadata = metadata(path);
        self.try_create_entry(name, metadata)
    }
}

impl LocalFileSystem {
    pub fn new() -> Self {
        Self {}
    }

    fn try_create_entry(
        &self,
        name: OsString,
        metadata: Result<Metadata, std::io::Error>,
    ) -> Result<Entry, std::io::Error> {
        let entry_name = match name.to_str() {
            Some(name) => name.to_string(),
            None => {
                return Err(std::io::Error::new(
                    ErrorKind::Other,
                    "Invalid file name".to_string(),
                ));
            }
        };

        let kind;
        let is_link;

        match metadata {
            Ok(metadata) => {
                if metadata.is_file() {
                    kind = EntryKind::File;
                } else {
                    kind = EntryKind::Directory;
                }
                is_link = metadata.is_symlink();
            }
            Err(err) => return Err(err),
        }

        Ok(Entry {
            name: entry_name,
            kind,
            is_link,
        })
    }
}
