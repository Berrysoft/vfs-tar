# vfs-tar
This is a working implementation that bridges [vfs](https://lib.rs/crates/vfs) and tar.
Internally it uses [memmap2](https://lib.rs/crates/memmap2) and a modified version of [a fork of tar-parser](https://github.com/nickelc/tar-parser.rs/tree/modernize).

## To-do list
- [x] Read-only file system.
- [x] Handle GNU long name.
- [x] Handle PAX.
- [x] Handle links.
- [ ] Make file system writable(?)
