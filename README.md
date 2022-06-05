# river

A very WIP Rust-based RISCV64 OS for learning.
The name is based off of the name RISCV with some added letters: "**ri**sc**v**" + **er**

Make sure you have submodules cloned, as it's required by `build-debug.sh` and `run-debug.sh`.
I have not provided a `build-release.sh` script as this is nowhere close to anything production-ready, and anyway the debug build has `-Copt-level=1` due to compiler bugs.

## Tips for debugging (especially before paging is enabled)

The [`.gdbinit`](.gdbinit) provided on this repository works very well for pre-paging debugging.
It requires [gef](https://github.com/hugsy/gef) to be installed at `~/.gdbinit-gef.py`.

Use the wrapper script [`run-debug.sh`][run-debug.sh], as it will launch QEMU and connect GDB to it.

Here's a quick rundown of some of my custom commands, only for debugging pre-paging code (after that, you can use typical commands--and even have the luxury of stack frames and panics):
- `addrof <symbol>` gives the physical kernel address of the symbol provided
- `set-break` is required to set a breakpoint to a symbol before paging is enabled
- `symof` gets the symbol name of a physical kernel address
- `lst` lists around the current physical kernel address
- `p2v` converts a physical kernel address to a virtual one (that can be used to look up addresses with `addr2line` and `info addr` & co.)
- `v2p` converts a virtual kernel address to a physical one (that can be used to examine memory with `x/...` when given a virtual address)
- `until_ret` is like `finish`, but just goes until the next `ret`
- `until_ret_ni` is `until_ret` then an `ni`
- `sio` steps over the next instruction (it does this by setting a breakpoint, disabling other breakpoints, and continuing, then reenabling other breakpoints), do note that it may re-enable breakpoints you had previously disabled, I have not checked the code completely as it was copied from StackOverflow (developer moment)

Virtual kernel addresses start with `0xFFFFFFD0` or higher, whereas physical kernel addresses start with `0x802`.

Once paging is enabled, you can generally use the addresses that GDB gives you. It's recommended, if you're debugging something after paging is enabled, which will be the case in most of the time, to do `break kmain`, then continue until `kmain`. This is the only breakpoint I've been able to set starting with a stopped QEMU, and once you get to it, you should be able to set any other breakpoint as the addresses will be correctly resolved in GDB's mind.

Additionally to virtual kernel addresses, there are 64GiB of virtual direct-mapped (i.e. contiguous pages) addresses starting at `0xFFFF_FFC0_0000_0000` mapped to `0x0` (physical memory) and ending at `0xFFFF_FFCF_C000_0000`, mapped to `0x10_0000_0000` (physical memory).

This can be helpful to know if you just need to probe or poke and prod at a specific address, as it will always be readable and writable.

(I'll probably add a command to help with converting direct-mapped addresses in the `.gdbinit` at some point.)

## Credits

- Thanks very much to [repnop](https://github.com/repnop) for creating [vanadinite](https://github.com/repnop/vanadinite), which has been helpful for learning.
- Additional thanks to the creators and contributors of [xv6-riscv](https://github.com/mit-pdos/xv6-riscv) which has been very helpful as a resource

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

Some files, explicitly marked with a header, are licensed individually under [MPL-2.0](https://www.mozilla.org/en-US/MPL/2.0/).
This is because they have a large portion of code taken from [vanadinite](https://github.com/repnop/vanadinite), licensed under these terms.
Any contribution to these marked files will be licensed under these terms.