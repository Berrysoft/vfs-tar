use tempfile::tempfile;
use vfs::VfsPath;
use vfs_tar::TarFS;

#[test]
fn basic() {
    let mut file = tempfile().unwrap();
    {
        let mut archive = tar_rs::Builder::new(&mut file);
        archive.append_dir_all("src", "src").unwrap();
        archive.finish().unwrap();
    }

    let fs = TarFS::from_std_file(&file).unwrap();
    let root = VfsPath::from(fs);
    let files = root
        .join("src")
        .unwrap()
        .read_dir()
        .unwrap()
        .map(|p| p.filename())
        .collect::<Vec<_>>();
    assert_eq!(&files, &["lib.rs"]);

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
