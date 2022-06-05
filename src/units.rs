// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2021 The vanadinite developers, 2022 ThePuzzlemaker
//
// This Source Code Form is subject to the terms of the Mozilla Public License,
// v. 2.0. If a copy of the MPL was not distributed with this file, You can
// obtain one at https://mozilla.org/MPL/2.0/.
//
// This file is based on code from vanadinite:
// https://github.com/repnop/vanadinite/blob/274659cc79955733147c5d391d0ee01993a2b945/src/kernel/vanadinite/src/utils.rs
// and is thus licensed independently from the rest of the project under MPL-2.0.

use core::ops::Mul;

pub trait StorageUnits: Mul<Self, Output = Self> + Sized {
    const KIB: Self;

    #[must_use]
    #[inline(always)]
    fn kib(self) -> Self {
        self * Self::KIB
    }

    #[must_use]
    #[inline(always)]
    fn mib(self) -> Self {
        self * Self::KIB * Self::KIB
    }

    #[must_use]
    #[inline(always)]
    fn gib(self) -> Self {
        self * Self::KIB * Self::KIB * Self::KIB
    }
}

macro_rules! impl_storage_units {
    ($($ty:ident),+$(,)?) => {
        $(
            impl StorageUnits for $ty {
                const KIB: Self = 1024;
            }
        )+
    }
}

impl_storage_units![u16, u32, u64, usize, i16, i32, i64, isize];
