#!/bin/env sh
set -e pipefail

# change this as necessary
env CROSS_COMPILE=riscv64-elf cargo build -Zbuild-std

pushd opensbi
make PLATFORM=generic LLVM=1 FW_PAYLOAD=../target/riscv64gc-unknown-none-elf/debug/river
popd
