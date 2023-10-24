// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2021 The vanadinite developers, 2023 ThePuzzlemaker
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
//! Some units that are used within rille and river. Right now this
//! just contains storage units.
//!
//! This module is based on code from [vanadinite][1] and is thus
//! licensed independently from the rest of the project under MPL-2.0.
//!
//! [1]: https://github.com/repnop/vanadinite/blob/274659cc79955733147c5d391d0ee01993a2b945/src/kernel/vanadinite/src/utils.rs

use core::ops::Mul;

/// A trait for integral types which implements helpful functions for
/// converting storage units.
pub trait StorageUnits: Mul<Self, Output = Self> + Sized {
    /// 1 kibibyte (KiB), in bytes.
    const KIB: Self;

    /// Convert `self` from kibibytes (KiB) to bytes.
    #[must_use]
    #[inline(always)]
    fn kib(self) -> Self {
        self * Self::KIB
    }

    /// Convert `self` from mebibytes (MiB) to bytes.
    #[must_use]
    #[inline(always)]
    fn mib(self) -> Self {
        self * Self::KIB * Self::KIB
    }

    /// Convert `self` from gibibytes (GiB) to bytes.
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

/// 1 kibibyte (KiB) is 1024 bytes.
pub const KIB: usize = 1024;
/// 1 mebibyte (MiB) is 1024 kibibytes (KiB).
pub const MIB: usize = KIB * KIB;
/// 1 gibibyte (MiB) is 1024 mebibytes (MiB)
pub const GIB: usize = KIB * KIB * KIB;

impl_storage_units![u16, u32, u64, usize, i16, i32, i64, isize];
