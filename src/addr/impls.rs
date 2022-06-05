use core::{fmt, hash};

use super::{Mapping, Mutability, Physical, Virtual};

impl<T, Map: Mapping, Mut: Mutability<T>> Copy for Physical<T, Map, Mut> {}
impl<T, Map: Mapping, Mut: Mutability<T>> Clone for Physical<T, Map, Mut> {
    fn clone(&self) -> Self {
        Self {
            addr: self.addr,
            _phantom: self._phantom,
        }
    }
}
impl<T, Map: Mapping, Mut: Mutability<T>> PartialEq for Physical<T, Map, Mut> {
    fn eq(&self, other: &Physical<T, Map, Mut>) -> bool {
        self.addr == other.addr
    }
}
impl<T, Map: Mapping, Mut: Mutability<T>> Eq for Physical<T, Map, Mut> {}
impl<T, Map: Mapping, Mut: Mutability<T>> fmt::Debug for Physical<T, Map, Mut> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Physical")
            .field("_map", &Map::default())
            .field("_mut", &Mut::default())
            .field("addr", &self.addr)
            .finish()
    }
}
impl<T, Map: Mapping, Mut: Mutability<T>> PartialOrd for Physical<T, Map, Mut> {
    fn partial_cmp(&self, other: &Physical<T, Map, Mut>) -> Option<core::cmp::Ordering> {
        self.addr.partial_cmp(&other.addr)
    }
}
impl<T, Map: Mapping, Mut: Mutability<T>> Ord for Physical<T, Map, Mut> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.addr.cmp(&other.addr)
    }
}
impl<T, Map: Mapping, Mut: Mutability<T>> hash::Hash for Physical<T, Map, Mut> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.addr.hash(state);
    }
}

impl<T, Map: Mapping, Mut: Mutability<T>> Copy for Virtual<T, Map, Mut> {}
impl<T, Map: Mapping, Mut: Mutability<T>> Clone for Virtual<T, Map, Mut> {
    fn clone(&self) -> Self {
        Self {
            addr: self.addr,
            _phantom: self._phantom,
        }
    }
}
impl<T, Map: Mapping, Mut: Mutability<T>> PartialEq for Virtual<T, Map, Mut> {
    fn eq(&self, other: &Virtual<T, Map, Mut>) -> bool {
        self.addr == other.addr
    }
}
impl<T, Map: Mapping, Mut: Mutability<T>> Eq for Virtual<T, Map, Mut> {}
impl<T, Map: Mapping, Mut: Mutability<T>> fmt::Debug for Virtual<T, Map, Mut> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Virtual")
            .field("addr", &self.addr)
            .field("_map", &Map::default())
            .field("_mut", &Mut::default())
            .finish()
    }
}
impl<T, Map: Mapping, Mut: Mutability<T>> PartialOrd for Virtual<T, Map, Mut> {
    fn partial_cmp(&self, other: &Virtual<T, Map, Mut>) -> Option<core::cmp::Ordering> {
        self.addr.partial_cmp(&other.addr)
    }
}
impl<T, Map: Mapping, Mut: Mutability<T>> Ord for Virtual<T, Map, Mut> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.addr.cmp(&other.addr)
    }
}
impl<T, Map: Mapping, Mut: Mutability<T>> hash::Hash for Virtual<T, Map, Mut> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.addr.hash(state);
    }
}
