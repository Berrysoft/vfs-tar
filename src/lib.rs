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

    /// Get the reference of the inner [`Mmap`].
    pub fn as_inner(&self) -> &Mmap {
        &self.file
    }

    /// Get the inner [`Mmap`].
    pub fn into_inner(self) -> Mmap {
        self.file
    }

    fn find_entry(&self, mut path: &str) -> Option<EntryRef> {
        loop {
            let res = Self::find_entry_impl(&self.root, strip_path(path).iter());
            if let Some(EntryRef::Link(p)) = res {
                path = p;
            } else {
                return res;
            }
        }
    }

    fn find_entry_impl<'a>(dir: &'a DirTree, mut path: Iter) -> Option<EntryRef<'a>> {
        let next_path = match path.next() {
            Some(str) => str.to_string_lossy(),
            None => return Some(EntryRef::Directory(dir)),
        };
        if let Some(entry) = dir.get(next_path.as_ref()) {
            match entry {
                Entry::File(buf) => {
                    debug_assert!(path.next().is_none());
                    Some(EntryRef::File(buf))
                }
                Entry::Directory(dir) => Self::find_entry_impl(dir, path),
                Entry::Link(p) => {
                    debug_assert!(path.next().is_none());
                    Some(EntryRef::Link(p))
                }
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
                EntryRef::Link(_) => unreachable!(),
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
    Link(&'static str),
}

#[derive(Debug)]
enum EntryRef<'a> {
    File(&'static [u8]),
    Directory(&'a DirTree),
    Link(&'static str),
}

type DirTree = HashMap<String, Entry>;

#[derive(Debug, Default)]
struct DirTreeBuilder {
    root: DirTree,
    longname: Option<Cow<'static, str>>,
    longlink: Option<&'static str>,
    realsize: Option<u64>,
}

impl DirTreeBuilder {
    pub fn build(mut self, entries: &[TarEntry<'static>]) -> DirTree {
        for entry in entries {
            match entry.header.typeflag {
                TypeFlag::Directory | TypeFlag::GnuDirectory => {
                    let name = self.get_name(entry);
                    self.insert_dir(Path::new(name.deref()));
                }
                TypeFlag::NormalFile | TypeFlag::ContiguousFile => {
                    let name = self.get_name(entry);
                    let size = self.realsize.take().unwrap_or(entry.header.size) as usize;
                    self.insert_file(Path::new(name.deref()), &entry.contents[..size])
                }
                TypeFlag::HardLink | TypeFlag::SymbolicLink => {
                    let name = self.get_name(entry);
                    let target = self.longlink.take().unwrap_or(entry.header.linkname);
                    self.insert_link(Path::new(name.deref()), target)
                }
                TypeFlag::GnuLongName => {
                    debug_assert!(entry.header.size > 1);
                    if let Ok((_, name)) = parse_long_name(entry.contents) {
                        debug_assert!(self.longname.is_none());
                        self.longname = Some(Cow::Borrowed(name));
                    }
                }
                TypeFlag::GnuLongLink => {
                    debug_assert!(entry.header.size > 1);
                    if let Ok((_, target)) = parse_long_name(entry.contents) {
                        debug_assert!(self.longlink.is_none());
                        self.longlink = Some(target);
                    }
                }
                TypeFlag::Pax => {
                    if let Ok((_, pax)) = parse_pax(entry.contents) {
                        if let Some(name) = pax.get("path") {
                            debug_assert!(self.longname.is_none());
                            self.longname = Some(Cow::Borrowed(name));
                        }
                        if let Some(target) = pax.get("linkpath") {
                            debug_assert!(self.longlink.is_none());
                            self.longlink = Some(target);
                        }
                        if let Some(size) = pax.get("size") {
                            debug_assert!(self.realsize.is_none());
                            self.realsize = size.parse().ok();
                        }
                    }
                }
                _ => {}
            }
        }
        self.root
    }

    fn get_name(&mut self, entry: &TarEntry<'static>) -> Cow<'static, str> {
        self.longname
            .take()
            .unwrap_or_else(|| Self::get_full_name(entry))
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
        if let Some(filename) = path.file_name() {
            current.insert(filename.to_string_lossy().into_owned(), Entry::File(buf));
        }
    }

    fn insert_link(&mut self, path: &Path, target: &'static str) {
        let current = if let Some(parent) = path.parent() {
            self.insert_dir(parent)
        } else {
            &mut self.root
        };
        if let Some(filename) = path.file_name() {
            current.insert(filename.to_string_lossy().into_owned(), Entry::Link(target));
        }
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

    #[test]
    fn link() {
        let name = "a".repeat(1024);
        let link_name = "b".repeat(1024);

        let file = tempfile().unwrap();
        let mut archive = tar_rs::Builder::new(file);
        archive.append_path_with_name("src/lib.rs", &name).unwrap();
        {
            let mut header = tar_rs::Header::new_gnu();
            header.set_entry_type(tar_rs::EntryType::Symlink);
            archive.append_link(&mut header, &link_name, &name).unwrap();
        }
        let file = archive.into_inner().unwrap();

        let fs = TarFS::from_std_file(&file).unwrap();
        let root = VfsPath::from(fs);

        let mut buffer = String::new();
        root.join(link_name)
            .unwrap()
            .open_file()
            .unwrap()
            .read_to_string(&mut buffer)
            .unwrap();
        let real_content = std::fs::read_to_string("src/lib.rs").unwrap();
        assert_eq!(buffer, real_content);
    }

    #[test]
    fn ustar() {
        let name = format!("{}/{}", "a".repeat(80), "b".repeat(80));
        let link_name = format!("{}/{}", "c".repeat(80), "d".repeat(80));

        let file = tempfile().unwrap();
        let mut archive = tar_rs::Builder::new(file);
        {
            let mut header = tar_rs::Header::new_ustar();
            let file = std::fs::File::open("src/lib.rs").unwrap();
            let size = file.metadata().unwrap().len();
            header.set_size(size);
            archive.append_data(&mut header, &name, file).unwrap();
        }
        {
            let mut header = tar_rs::Header::new_ustar();
            header.set_entry_type(tar_rs::EntryType::Symlink);
            archive.append_link(&mut header, &link_name, &name).unwrap();
        }
        let file = archive.into_inner().unwrap();

        let fs = TarFS::from_std_file(&file).unwrap();
        let root = VfsPath::from(fs);

        let mut buffer = String::new();
        root.join(link_name)
            .unwrap()
            .open_file()
            .unwrap()
            .read_to_string(&mut buffer)
            .unwrap();
        let real_content = std::fs::read_to_string("src/lib.rs").unwrap();
        assert_eq!(buffer, real_content);
    }
}
