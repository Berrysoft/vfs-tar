use tempfile::tempfile;
use vfs::VfsPath;
use vfs_tar::TarFS;

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
