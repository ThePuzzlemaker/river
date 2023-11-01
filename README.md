# river

River is an experimental capability-based operating system written in
Rust for RISCV64. (For those wondering, the name is taken from RISC-V:
"**RI**sc**V**" + "**ER**"). This project is not intended to be used
in production, but instead just as a project for learning about
advanced OS concepts and existing kernels.

River takes inspiration from [repnop][1]'s [Vanadinite][2], the
[seL4][3] project, [GWU Composite][4] (which has some [very helpful
accompanying lectures][5]), [Fucshia/Zircon][6], and
[XV6/RISC-V][7]. Thanks goes to all the amazing people who have helped
me through my insanity as I try to figure this stuff out.

[1]: https://github.com/repnop
[2]: https://github.com/repnop/vanadinite
[3]: https://seL4.systems
[4]: https://github.com/gwsystems/composite
[5]: https://www.youtube.com/watch?v=a8V2d33KvaE&list=PLVW70f0xtTUwPxQXXcQBZJps-7n8BclOc
[6]: https://fuchsia.dev/fuchsia-src/concepts/kernel
[7]: https://github.com/mit-pdos/xv6-riscv

## Rille

Rille is to River as `libc` is to Linux. It's a somewhat opinionated
interface library with some additional helpful primitives for building
applications. For more information, you can see the [module's
documentation](rille/src/lib.rs).

For those wondering, the name derives from the river-like structures
found on the moon (and other celestial bodies) called rilles.

## Building

First, ensure submodules are cloned. Then, use `cargo xtask build` to
build. See the help options, or see
[`xtask/src/main.rs`](xtask/src/main.rs) for help or more info.

### Tips for debugging (especially before paging is enabled)

The [`.gdbinit`](.gdbinit) provided on this repository works very well
for pre-paging debugging.  It requires
[gef](https://github.com/hugsy/gef) to be installed at
`~/.gdbinit-gef.py`. If you're going to debug userspace programs, set
a breakpoint at the start of the user program. Just note that it'll
step right into the kernel if you let it, so you may want to set a
breakpoint at the end of the trampoline (`river::trap::user_ret`). For
debugging the kernel after paging, it helps to break at `kmain`,
continue, then set a breakpoint at your desired position.

Use the wrapper script [`run-debug.sh`](run-debug.sh), as it will
launch QEMU and connect GDB to it.

Here's a quick rundown of some of my custom commands, only for
debugging pre-paging code (after that, you can use typical
commands--and even have the luxury of stack frames and panics):
- `addrof <symbol>` gives the physical kernel address of the symbol
  provided
- `set-break` is required to set a breakpoint to a symbol before
  paging is enabled
- `symof` gets the symbol name of a physical kernel address
- `lst` lists around the current physical kernel address
- `p2v` converts a physical kernel address to a virtual one (that can
  be used to look up addresses with `addr2line` and `info addr` & co.)
- `v2p` converts a virtual kernel address to a physical one (that can
  be used to examine memory with `x/...` when given a virtual address)
- `until_ret` is like `finish`, but just goes until the next `ret`
- `until_ret_ni` is `until_ret` then an `ni`
- `sio` steps over the next instruction (it does this by setting a
  breakpoint, disabling other breakpoints, and continuing, then
  reenabling other breakpoints), do note that it may re-enable
  breakpoints you had previously disabled, I have not checked the code
  completely as it was copied from StackOverflow (developer moment)

Virtual kernel addresses start with `0xFFFFFFD0` or higher, whereas
physical kernel addresses start with `0x802`.

Once paging is enabled, you can generally use the addresses that GDB
gives you. It's recommended, if you're debugging something after
paging is enabled, which will be the case in most of the time, to do
`break kmain`, then continue until `kmain`. This is the only
breakpoint I've been able to set starting with a stopped QEMU, and
once you get to it, you should be able to set any other breakpoint as
the addresses will be correctly resolved in GDB's mind.

Additionally to virtual kernel addresses, there are 64GiB of virtual
direct-mapped (i.e. contiguous pages) addresses starting at
`0xFFFF_FFC0_0000_0000` mapped to `0x0` (physical memory) and ending
at `0xFFFF_FFCF_C000_0000`, mapped to `0x10_0000_0000` (physical
memory).

This can be helpful to know if you just need to probe or poke and prod
at a specific address, as it will always be readable and writable.

(I'll probably add a command to help with converting direct-mapped
addresses in the `.gdbinit` at some point.)

## License

Licensed under either of

 * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the
Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.

Some files, explicitly marked with a header, are licensed individually
under [MPL-2.0](https://www.mozilla.org/en-US/MPL/2.0/). This is
because they have a large portion of code taken from
[vanadinite](https://github.com/repnop/vanadinite), licensed under
these terms. Any contribution to these marked files will be licensed
under these terms.
