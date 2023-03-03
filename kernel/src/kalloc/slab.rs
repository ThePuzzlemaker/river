// putting this allocator on the back-burner for now.
// use core::{
//     alloc::Layout,
//     any,
//     cell::Cell,
//     marker::PhantomData,
//     mem,
//     ptr::{self, addr_of_mut, NonNull},
//     sync::atomic::{self, AtomicUsize, Ordering},
// };

// use crate::{addr::VirtualMut, hart_local::HartLocal, spin::SpinMutex};

// use super::phys::PMAlloc;

// #[derive(Debug)]
// pub struct PerHartCache {
//     free_list: Cell<Option<NonNull<ObjectNode>>>,
//     allocd_objects: Cell<Option<NonNull<AtomicUsize>>>,
// }

// #[derive(Debug)]
// #[repr(C)]
// pub struct SlabCache<T> {
//     per_hart: &'static HartLocal<PerHartCache>,
//     partial: SpinMutex<Option<NonNull<Slab>>>,
//     full: SpinMutex<Option<NonNull<Slab>>>,
//     layout: Layout,
//     _phantom: PhantomData<*mut T>,
// }

// unsafe impl<T> Sync for SlabCache<T> {}

// impl<T> SlabCache<T> {
//     #[track_caller]
//     pub fn new(per_hart: &'static HartLocal<PerHartCache>) -> Self {
//         let layout = Layout::new::<T>();
//         assert!(
//             4096 - layout.pad_to_align().size() >= mem::size_of::<Slab>()
//                 && layout.align() <= 4096 - mem::size_of::<Slab>(),
//             "SlabCache::<{}>::new: type will not fit within the slab (must be less than 4KiB large)",
//             any::type_name::<T>()
//         );
//         Self {
//             per_hart,
//             partial: SpinMutex::new(None),
//             full: SpinMutex::new(None),
//             layout: Layout::new::<T>(),
//             _phantom: PhantomData,
//         }
//     }

//     #[cfg_attr(debug_assertions, track_caller)]
//     pub fn init_hart(&self) {
//         debug_assert!(
//             self.per_hart.with(|cache| cache.free_list.get().is_none()),
//             "SlabCache::init_hart: already initialized"
//         );

//         self.take_or_make_cache();
//     }

//     #[allow(clippy::drop_ref)]
//     fn take_or_make_cache(&self) {
//         let mut partial = self.partial.lock();
//         let slab_ptr = if let Some(partial_ref) = &mut *partial {
//             // There's a free partial slab, put it in this cache.
//             *partial_ref
//         } else {
//             // We have to make a new cache.
//             Slab::new(self.layout)
//         };
//         // SAFETY: We have exclusive access to this slab.
//         let slab = unsafe { &mut *slab_ptr.as_ptr() };

//         let tmp_next = slab.next.take();
//         // Take the free list of the slab and make it ours.
//         let tmp_free_list = slab.free_list.take();
//         // Get the allocd_objects and get a pointer to it.
//         // SAFETY: We know this pointer is valid and non-null.
//         let tmp_allocd_objects =
//             unsafe { NonNull::new_unchecked(&slab.allocd_objects as *const _ as *mut _) };

//         drop(slab);

//         if partial.is_some() {
//             // Splice the partial slab out of the list, and set it as our cache.
//             *partial = tmp_next;
//         } else {
//             // Put the partial slab at the head of the list.
//             *partial = Some(slab_ptr);
//         }

//         self.per_hart.with(|cache| {
//             cache.free_list.set(tmp_free_list);
//             cache.allocd_objects.set(Some(tmp_allocd_objects));
//         });
//     }
// }

// #[derive(Debug)]
// #[repr(C)]
// pub struct Slab {
//     pub(crate) free_list: Option<NonNull<ObjectNode>>,
//     next: Option<NonNull<Slab>>,
//     prev: Option<NonNull<Slab>>,
//     allocd_objects: AtomicUsize,
// }

// impl Slab {
//     pub fn new(layout: Layout) -> NonNull<Self> {
//         let mut pma = PMAlloc::get();
//         let page = pma
//             .allocate(0)
//             .expect("Slab::new: failed to allocate a slab");
//         let layout = layout.pad_to_align();
//         // SAFETY: PMAlloc is guaranteed to give us a non-null address.
//         let ptr: NonNull<Slab> =
//             unsafe { NonNull::new_unchecked(page.into_virt().into_ptr_mut()) }.cast();
//         let slab = unsafe {
//             // SAFETY: We have exclusive access to ptr.
//             let slab = ptr.as_uninit_mut();
//             let slab_ptr = slab.as_mut_ptr();
//             addr_of_mut!((*slab_ptr).free_list).write(None);
//             addr_of_mut!((*slab_ptr).next).write(None);
//             addr_of_mut!((*slab_ptr).prev).write(None);
//             addr_of_mut!((*slab_ptr).allocd_objects).write(AtomicUsize::new(0));
//             // SAFETY: We have just initialized this struct.
//             slab.assume_init_mut()
//         };
//         let next = unsafe { ptr.as_ptr().add(1) }.cast::<u8>();
//         let mut prev: *mut u8 = ptr::null_mut();
//         let mut next: *mut u8 = unsafe { next.add(next.align_offset(layout.align())) };
//         slab.free_list = Some(unsafe { NonNull::new_unchecked(next.cast()) });
//         loop {
//             if next.is_null() {
//                 break;
//             }

//             // Make sure we don't overflow our page.
//             if next as usize >= ptr.as_ptr() as usize + 4096 {
//                 next = ptr::null_mut();
//             }

//             if !prev.is_null() {
//                 let node = unsafe {
//                     NonNull::new_unchecked(prev)
//                         .cast::<ObjectNode>()
//                         .as_uninit_mut()
//                 };
//                 let node_ptr = node.as_mut_ptr();
//                 unsafe { addr_of_mut!((*node_ptr).next).write(NonNull::new(next.cast())) };
//             }

//             prev = next;
//             next = unsafe { next.add(layout.size()) };
//         }

//         ptr
//     }

//     unsafe fn deallocate(ptr: NonNull<Self>) {
//         #[cfg(debug_assertions)]
//         {
//             let zelf = ptr.as_ref();
//             let allocd_objects = zelf.allocd_objects.load(Ordering::Acquire);
//             assert!(
//                 allocd_objects == 0,
//                 "Slab::deallocate: attempted to deallocate a slab with {:?} remaining objects",
//                 allocd_objects
//             );
//             atomic::fence(Ordering::Release);
//         }
//         let mut pma = PMAlloc::get();
//         pma.deallocate(VirtualMut::from_ptr(ptr.as_ptr()).into_phys().cast(), 0);
//     }
// }

// #[derive(Debug)]
// #[repr(C)]
// pub struct ObjectNode {
//     pub(crate) next: Option<NonNull<ObjectNode>>,
// }
