# vfs-tar
This is a working implementation that bridges [vfs](https://lib.rs/crates/vfs) and tar.
Internally it uses [memmap2](https://lib.rs/crates/memmap2) and a modified version of [a fork of tar-parser](https://github.com/nickelc/tar-parser.rs/tree/modernize).

## To-do list
- [x] Readonly filesystem.
- [x] Handle more file types. May give detailed errors.
- [ ] Make filesystem writable.
