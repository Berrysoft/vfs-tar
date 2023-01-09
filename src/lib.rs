//! This is a bridge of [`vfs`] and TAR files.

#![warn(missing_docs)]

mod parser;

use memmap2::{Mmap, MmapOptions};
use parser::*;
use std::{
    borrow::Cow,
    collections::HashMap,
    fs::File,
    io::{Cursor, Write},
    ops::Deref,
    path::{Iter, Path},
};
use vfs::{error::VfsErrorKind, *};

/// A readonly tar archive filesystem.
#[derive(Debug)]
pub struct TarFS {
    #[allow(dead_code)]
    file: Mmap,
    root: DirTree,
}

impl TarFS {
    /// Create [`TarFS`] from the archive path.
    pub fn new(p: impl AsRef<Path>) -> VfsResult<Self> {
        Self::from_std_file(&File::open(p)?)
    }

    /// Create [`TarFS`] from [`File`].
    /// Note that the filesystem is still valid after the [`File`] being dropped.
    pub fn from_std_file(f: &File) -> VfsResult<Self> {
        // SAFETY: mmap with COW
        let file = unsafe { MmapOptions::new().map_copy_read_only(f) }?;
        // SAFETY: the entries won't live longer than mmap
        let (_, entries) = parse_tar(unsafe { &*(file.deref() as *const [u8]) })
            .map_err(|e| VfsErrorKind::Other(e.to_string()))?;
        let root = DirTreeBuilder::default().build(&entries);
        Ok(Self { file, root })
    }

    fn find_entry(&self, path: &str) -> Option<EntryRef> {
        Self::find_entry_impl(&self.root, strip_path(path).iter())
    }

    fn find_entry_impl<'a>(dir: &'a DirTree, mut path: Iter) -> Option<EntryRef<'a>> {
        let next_path = match path.next() {
            Some(str) => str.to_string_lossy(),
            None => return Some(EntryRef::Directory(dir)),
        };
        if let Some(entry) = dir.get(next_path.as_ref()) {
            match entry {
                Entry::File(buf) => Some(EntryRef::File(buf)),
                Entry::Directory(dir) => Self::find_entry_impl(dir, path),
            }
        } else {
            None
        }
    }
}

impl FileSystem for TarFS {
    fn read_dir(&self, path: &str) -> VfsResult<Box<dyn Iterator<Item = String> + Send>> {
        match self.find_entry(path) {
            Some(EntryRef::Directory(dir)) => Ok(Box::new(
                dir.keys()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .into_iter(),
            )),
            _ => Err(VfsErrorKind::FileNotFound.into()),
        }
    }

    fn create_dir(&self, _path: &str) -> VfsResult<()> {
        Err(VfsErrorKind::NotSupported.into())
    }

    fn open_file(&self, path: &str) -> VfsResult<Box<dyn SeekAndRead + Send>> {
        match self.find_entry(path) {
            Some(EntryRef::File(buf)) => Ok(Box::new(Cursor::new(buf))),
            _ => Err(VfsErrorKind::FileNotFound.into()),
        }
    }

    fn create_file(&self, _path: &str) -> VfsResult<Box<dyn Write + Send>> {
        Err(VfsErrorKind::NotSupported.into())
    }

    fn append_file(&self, _path: &str) -> VfsResult<Box<dyn Write + Send>> {
        Err(VfsErrorKind::NotSupported.into())
    }

    fn metadata(&self, path: &str) -> VfsResult<VfsMetadata> {
        match self.find_entry(path) {
            Some(e) => match e {
                EntryRef::File(buf) => Ok(VfsMetadata {
                    file_type: VfsFileType::File,
                    len: buf.len() as u64,
                }),
                EntryRef::Directory(_) => Ok(VfsMetadata {
                    file_type: VfsFileType::Directory,
                    len: 0,
                }),
            },
            None => Err(VfsErrorKind::FileNotFound.into()),
        }
    }

    fn exists(&self, path: &str) -> VfsResult<bool> {
        Ok(self.find_entry(path).is_some())
    }

    fn remove_file(&self, _path: &str) -> VfsResult<()> {
        Err(VfsErrorKind::NotSupported.into())
    }

    fn remove_dir(&self, _path: &str) -> VfsResult<()> {
        Err(VfsErrorKind::NotSupported.into())
    }
}

#[derive(Debug)]
enum Entry {
    File(&'static [u8]),
    Directory(DirTree),
}

#[derive(Debug)]
enum EntryRef<'a> {
    File(&'static [u8]),
    Directory(&'a DirTree),
}

type DirTree = HashMap<String, Entry>;

#[derive(Debug, Default)]
struct DirTreeBuilder {
    root: DirTree,
}

impl DirTreeBuilder {
    pub fn build(mut self, entries: &[TarEntry<'static>]) -> DirTree {
        let mut longname = None;
        let mut realsize = None;
        for entry in entries {
            let name = longname
                .take()
                .unwrap_or_else(|| Self::get_full_name(entry));
            match entry.header.typeflag {
                TypeFlag::Directory | TypeFlag::GnuDirectory => {
                    self.insert_dir(Path::new(name.deref()));
                }
                TypeFlag::NormalFile | TypeFlag::ContiguousFile => self.insert_file(
                    Path::new(name.deref()),
                    &entry.contents[..realsize.take().unwrap_or(entry.header.size) as usize],
                ),
                TypeFlag::GnuLongName => {
                    debug_assert!(longname.is_none());
                    debug_assert!(entry.header.size > 1);
                    longname = Some(Cow::Borrowed(parse_long_name(entry.contents).unwrap().1));
                }
                TypeFlag::Pax => {
                    let pax = parse_pax(entry.contents).unwrap().1;
                    if let Some(name) = pax.get("path") {
                        debug_assert!(longname.is_none());
                        longname = Some(Cow::Borrowed(name));
                    }
                    if let Some(size) = pax.get("size") {
                        debug_assert!(realsize.is_none());
                        realsize = size.parse().ok();
                    }
                }
                _ => {}
            }
        }
        self.root
    }

    fn get_full_name(entry: &TarEntry<'static>) -> Cow<'static, str> {
        if let ExtraHeader::UStar(ustar) = &entry.header.ustar {
            if let UStarExtraHeader::Posix(header) = &ustar.extra {
                if !header.prefix.is_empty() {
                    return Cow::Owned(format!("{}/{}", header.prefix, entry.header.name));
                }
            }
        };
        Cow::Borrowed(entry.header.name)
    }

    fn insert_dir(&mut self, path: &Path) -> &mut DirTree {
        let path = path.iter();
        let mut current = &mut self.root;
        for p in path {
            let entry = current
                .entry(p.to_string_lossy().into_owned())
                .or_insert_with(|| Entry::Directory(DirTree::new()));
            current = if let Entry::Directory(dir) = entry {
                dir
            } else {
                unreachable!()
            };
        }
        current
    }

    fn insert_file(&mut self, path: &Path, buf: &'static [u8]) {
        let current = if let Some(parent) = path.parent() {
            self.insert_dir(parent)
        } else {
            &mut self.root
        };
        current.insert(
            path.file_name().unwrap().to_string_lossy().into_owned(),
            Entry::File(buf),
        );
    }
}

/// [`Path`] doesn't iterate well with the prefix `/`.
fn strip_path(path: &str) -> &Path {
    Path::new(path.strip_prefix('/').unwrap_or(path))
}

#[cfg(test)]
mod test {
    use crate::TarFS;
    use tempfile::tempfile;
    use vfs::VfsPath;

    #[test]
    fn basic() {
        let file = tempfile().unwrap();
        let mut archive = tar_rs::Builder::new(file);
        archive.append_dir_all("src", "src").unwrap();
        let file = archive.into_inner().unwrap();

        let fs = TarFS::from_std_file(&file).unwrap();
        let root = VfsPath::from(fs);
        let mut files = root
            .join("src")
            .unwrap()
            .read_dir()
            .unwrap()
            .map(|p| p.filename())
            .collect::<Vec<_>>();
        files.sort();
        assert_eq!(&files, &["lib.rs", "parser.rs"]);

        let mut buffer = String::new();
        root.join("src/lib.rs")
            .unwrap()
            .open_file()
            .unwrap()
            .read_to_string(&mut buffer)
            .unwrap();
        let real_content = std::fs::read_to_string("src/lib.rs").unwrap();
        assert_eq!(buffer, real_content);
    }

    #[test]
    fn long() {
        let name = "a".repeat(1024);

        let file = tempfile().unwrap();
        let mut archive = tar_rs::Builder::new(file);
        archive.append_path_with_name("src/lib.rs", &name).unwrap();
        let file = archive.into_inner().unwrap();

        let fs = TarFS::from_std_file(&file).unwrap();
        let root = VfsPath::from(fs);

        let mut buffer = String::new();
        root.join(name)
            .unwrap()
            .open_file()
            .unwrap()
            .read_to_string(&mut buffer)
            .unwrap();
        let real_content = std::fs::read_to_string("src/lib.rs").unwrap();
        assert_eq!(buffer, real_content);
    }
}
