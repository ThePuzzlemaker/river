use core::{
    ptr::NonNull,
    sync::atomic::{self, AtomicU64, Ordering},
};

/// This trait genericizes `Arc<T>` to any pointer that can access an
/// atomic refcount.
///
/// # Safety
///
/// The data must only be synchronized using this `ArcLike`
/// type. Additionally, all unsafe functions must be implemented with
/// the proper invariants.
pub unsafe trait ArcLike
where
    Self: Sized,
{
    type Target: ?Sized;

    fn refcount(arc: &Self) -> &AtomicU64;

    fn data(arc: &Self) -> NonNull<Self::Target>;

    /// TODO
    ///
    /// # Safety
    ///
    /// - This must only be deallocated once the refcount is 1 and the
    /// last object is dropped, i.e. do not call this manually--it is
    /// handled by Drop.
    ///
    /// - This must not modify the reference count.
    unsafe fn deallocate(arc: &mut Self);

    /// TODO
    ///
    /// # Safety
    ///
    /// - This must only be called when the refcount has already been
    /// increased.
    unsafe fn clone_ref(arc: &Self) -> Self;

    fn clone_impl(arc: &Self) -> Self {
        assert!(
            Self::refcount(arc).fetch_add(1, Ordering::Relaxed) <= u64::MAX / 2,
            "ArcLike: too many references"
        );

        // SAFETY: We have increased the refcount.
        unsafe { Self::clone_ref(arc) }
    }

    fn drop_impl(arc: &mut Self) {
        // Release-store the decrement.
        if Self::refcount(arc).fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        // If we were the last, we acquire-fence so that every
        // decrement and use of the inner data happens-before this
        // deallocation.
        atomic::fence(Ordering::Acquire);
        // SAFETY: We have the last reference, and we know that this
        // arc is still valid.
        unsafe {
            Self::deallocate(arc);
        }
    }

    fn deref_impl(arc: &Self) -> &Self::Target {
        // SAFETY: Our invariants ensure this is safe.
        unsafe { Self::data(arc).as_ref() }
    }

    unsafe fn increase_refcount(&self) {
        assert!(
            Self::refcount(self).fetch_add(1, Ordering::Relaxed) <= u64::MAX / 2,
            "ArcLike: too many references"
        );
    }
}

#[macro_export]
macro_rules! impl_arclike_traits {
    ($ident:ident) => {
        impl ::core::ops::Deref for $ident {
            type Target = <Self as $crate::sync::arc_like::ArcLike>::Target;

            fn deref(&self) -> &Self::Target {
                <Self as $crate::sync::arc_like::ArcLike>::deref_impl(self)
            }
        }

        impl ::core::clone::Clone for $ident {
            fn clone(&self) -> Self {
                <Self as $crate::sync::arc_like::ArcLike>::clone_impl(self)
            }
        }

        impl ::core::ops::Drop for $ident {
            fn drop(&mut self) {
                <Self as $crate::sync::arc_like::ArcLike>::drop_impl(self)
            }
        }
    };
}
