#!/bin/env sh
set -e pipefail

# change this as necessary
env CROSS_COMPILE=riscv64-elf cargo doc --document-private-items -Zbuild-std=core,alloc,compiler_builtins -Zbuild-std-features=compiler-builtins-mem
