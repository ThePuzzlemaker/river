#!/bin/env sh
set -e pipefail

chmod +x ./build-debug.sh
./build-debug.sh

qemu-system-riscv64 -M virt -m 256M \
    -bios opensbi/build/platform/generic/firmware/fw_jump.bin \
    -kernel target/riscv64gc-unknown-none-elf/debug/river \
    -serial stdio \
    -d int -s -S &

riscv64-elf-gdb target/riscv64gc-unknown-none-elf/debug/river

kill -INT %1