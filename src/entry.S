/*
    SPDX-License-Identifier: MPL-2.0
    SPDX-FileCopyrightText: 2021 The vanadinite developers, 2022 ThePuzzlemaker
    
    This Source Code Form is subject to the terms of the Mozilla Public License,
    v. 2.0. If a copy of the MPL was not distributed with this file, You can
    obtain one at https://mozilla.org/MPL/2.0/.

    This file contains a large portion of code taken from vanadinite:
    https://github.com/repnop/vanadinite/blob/274659cc79955733147c5d391d0ee01993a2b945/src/kernel/vanadinite/src/boot/entry.rs
    and is thus licensed individually to the rest of the project as MPL-2.0.
*/
.section .init

.option norvc

.type start, @function
.global start
start:
    # Disable S-mode interrupts
    csrw sie, zero
    csrci sstatus, 2

.option push
.option norelax
    lla gp, __global_pointer$
.option pop

    lla t0, __bss_start
    lla t1, __bss_end

    # Clear the .bss section
clear_bss:
    beq t0, t1, done_clear_bss
    sd zero, (t0)
    addi t0, t0, 8
    j clear_bss
    
done_clear_bss:
    lla sp, __tmp_stack_top

    # Put the hart id into the thread pointer.
    # We do not have TLS so the thread pointer is unused otherwise.
    # It's saved/restored on context switches to/from usermode too.
    mv tp, a0

    # We don't care about having the hart id in a0 anymore, so let's move the
    # FDT pointer as well and just give the kernel entry point just one
    # argument.
    mv a0, a1

    tail early_boot