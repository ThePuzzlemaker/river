use core::{fmt, hash};

use super::{Mapping, Mutability, Physical, Virtual};

impl<T, Map: Mapping, Mut: Mutability> Copy for Physical<T, Map, Mut> {}

impl<T, Map: Mapping, Mut: Mutability> Clone for Physical<T, Map, Mut> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, Map: Mapping, Mut: Mutability> PartialEq for Physical<T, Map, Mut> {
    fn eq(&self, other: &Self) -> bool {
        self.addr == other.addr
    }
}

impl<T, Map: Mapping, Mut: Mutability> Eq for Physical<T, Map, Mut> {}

impl<T, Map: Mapping, Mut: Mutability> fmt::Debug for Physical<T, Map, Mut> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Physical")
            .field("_map", &Map::default())
            .field("_mut", &Mut::default())
            .field("addr", &self.addr)
            .finish()
    }
}

impl<T, Map: Mapping, Mut: Mutability> PartialOrd for Physical<T, Map, Mut> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.addr.cmp(&other.addr))
    }
}

impl<T, Map: Mapping, Mut: Mutability> Ord for Physical<T, Map, Mut> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.addr.cmp(&other.addr)
    }
}


impl<T, Map: Mapping, Mut: Mutability> hash::Hash for Physical<T, Map, Mut> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.addr.hash(state);
    }
}

impl<T, Map: Mapping, Mut: Mutability> Copy for Virtual<T, Map, Mut> {}

impl<T, Map: Mapping, Mut: Mutability> Clone for Virtual<T, Map, Mut> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, Map: Mapping, Mut: Mutability> PartialEq for Virtual<T, Map, Mut> {
    fn eq(&self, other: &Self) -> bool {
        self.addr == other.addr
    }
}

impl<T, Map: Mapping, Mut: Mutability> Eq for Virtual<T, Map, Mut> {}

impl<T, Map: Mapping, Mut: Mutability> fmt::Debug for Virtual<T, Map, Mut> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Virtual")
            .field("addr", &self.addr)
            .field("_map", &Map::default())
            .field("_mut", &Mut::default())
            .finish()
    }
}

impl<T, Map: Mapping, Mut: Mutability> PartialOrd for Virtual<T, Map, Mut> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.addr.cmp(&other.addr))
    }
}

impl<T, Map: Mapping, Mut: Mutability> Ord for Virtual<T, Map, Mut> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.addr.cmp(&other.addr)
    }
}

impl<T, Map: Mapping, Mut: Mutability> hash::Hash for Virtual<T, Map, Mut> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.addr.hash(state);
    }
}
