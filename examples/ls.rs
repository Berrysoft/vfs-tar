use vfs::{VfsFileType, VfsPath};
use vfs_tar::TarFS;

fn main() {
    let path = std::env::args_os().nth(1).unwrap();
    let root: VfsPath = TarFS::new_mmap(path).unwrap().into();
    read_dir(&root);
}

fn read_dir(p: &VfsPath) {
    let mut dir_name = p.as_str();
    if dir_name.is_empty() {
        dir_name = "/";
    }
    println!("D {dir_name}");
    for entry in p.read_dir().unwrap() {
        let metadata = entry.metadata().unwrap();
        match metadata.file_type {
            VfsFileType::Directory => read_dir(&entry),
            VfsFileType::File => println!("F {} {}", entry.as_str(), metadata.len),
        }
    }
}
