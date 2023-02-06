# vfs-tar
This is a working implementation that bridges [vfs](https://lib.rs/crates/vfs) and tar.
Internally it uses [memmap2](https://lib.rs/crates/memmap2) and [tar-parser2](https://lib.rs/crates/tar-parser2).

## To-do list
- [x] Read-only file system.
- [x] Handle GNU long name.
- [x] Handle PAX.
- [x] Handle links.
- [ ] Make file system writable(?)
