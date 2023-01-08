use memmap2::{Mmap, MmapOptions};
use std::{
    collections::HashMap,
    fs::File,
    io::{Cursor, Write},
    ops::Deref,
    path::{Iter, Path},
};
use tar::{parse_tar, TarEntry, TypeFlag};
use vfs::{error::VfsErrorKind, *};

#[derive(Debug)]
pub struct TarFS {
    #[allow(dead_code)]
    file: Mmap,
    root: DirTree,
}

impl TarFS {
    pub fn new(p: impl AsRef<Path>) -> VfsResult<Self> {
        Self::from_std_file(&File::open(p)?)
    }

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

type DirTree = HashMap<&'static str, Entry>;

#[derive(Debug, Default)]
struct DirTreeBuilder {
    root: DirTree,
}

impl DirTreeBuilder {
    pub fn build(mut self, entries: &[TarEntry<'static>]) -> DirTree {
        for entry in entries {
            match entry.header.typeflag {
                TypeFlag::Directory => {
                    self.insert_dir(Path::new(entry.header.name));
                }
                TypeFlag::NormalFile => self.insert_file(
                    Path::new(entry.header.name),
                    &entry.contents[..entry.header.size as usize],
                ),
                _ => unimplemented!(),
            }
        }
        self.root
    }

    fn insert_dir(&mut self, path: &'static Path) -> &mut DirTree {
        let path = path.iter();
        let mut current = &mut self.root;
        for p in path {
            let entry = current
                .entry(p.to_str().unwrap())
                .or_insert_with(|| Entry::Directory(DirTree::new()));
            current = if let Entry::Directory(dir) = entry {
                dir
            } else {
                unreachable!()
            };
        }
        current
    }

    fn insert_file(&mut self, path: &'static Path, buf: &'static [u8]) {
        let current = self.insert_dir(path.parent().unwrap());
        current.insert(
            path.file_name().unwrap().to_str().unwrap(),
            Entry::File(buf),
        );
    }
}

fn strip_path(path: &str) -> &Path {
    Path::new(path.strip_prefix('/').unwrap_or(path))
}
