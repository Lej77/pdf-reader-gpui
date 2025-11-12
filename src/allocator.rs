#![expect(dead_code, reason = "we don't use all of the methods")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::backtrace::Backtrace;
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[global_allocator]
pub static GLOBAL: ThreadLocalAlloc = ThreadLocalAlloc;

pub struct DynAlloc<'a>(pub &'a dyn GlobalAlloc);
unsafe impl<'a> GlobalAlloc for DynAlloc<'a> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { self.0.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { self.0.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe { self.0.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe { self.0.realloc(ptr, layout, new_size) }
    }
}

thread_local! {
    static CURRNET_ALLOCATOR: Cell<Option<&'static dyn GlobalAlloc>> = const { Cell::new(None) };
}
pub struct ThreadLocalAlloc;
impl ThreadLocalAlloc {
    /// # Safety
    ///
    /// - If the new allocator works differently from the previous allocator then:
    ///   - All memory allocated inside the closure must be freed before the closure returns, or leaked forever.
    ///   - Don't free memory allocated before this call inside the closure.
    pub unsafe fn with_allocator<F, R>(allocator: &dyn GlobalAlloc, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        CURRNET_ALLOCATOR.with(|cell| {
            struct TempUsed<'a> {
                cell: &'a Cell<Option<&'static dyn GlobalAlloc>>,
                prev: Option<&'static dyn GlobalAlloc>,
            }
            impl Drop for TempUsed<'_> {
                fn drop(&mut self) {
                    self.cell.set(self.prev);
                }
            }

            // Safety: the allocator is only used during the call
            let static_allocator: &'static dyn GlobalAlloc = unsafe {
                &*(allocator as *const (dyn GlobalAlloc + '_) as *const (dyn GlobalAlloc + 'static))
            };
            let prev = cell.replace(Some(static_allocator));
            let _guard = TempUsed { cell, prev };

            f()
        })
    }
    pub fn with_no_leaks<R>(f: impl FnOnce(&TrackingAlloc<DynAlloc>) -> R) -> R {
        let allocator =
            TrackingAlloc::with_allocator(DynAlloc(CURRNET_ALLOCATOR.get().unwrap_or(&System)));
        let f = || {
            struct CheckNoLeak<'a, T: GlobalAlloc>(&'a TrackingAlloc<T>);
            impl<'a, T: GlobalAlloc> Drop for CheckNoLeak<'a, T> {
                fn drop(&mut self) {
                    self.0.forget_and_warn_all();
                }
            }
            let _guard = CheckNoLeak(&allocator);

            catch_unwind(AssertUnwindSafe(|| f(&allocator))).map_err(|_| ())
        };

        // Safety:
        // - we wrap the outer scopes allocator so the allocation behavior should remain the same.
        let result = unsafe { ThreadLocalAlloc::with_allocator(&allocator, f) };

        result.unwrap_or_else(|_| {
            std::panic::resume_unwind(Box::new(
                "resumed panic after ThreadLocalAlloc::with_no_leaks",
            ))
        })
    }
}
unsafe impl GlobalAlloc for ThreadLocalAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if let Ok(Some(memory)) = CURRNET_ALLOCATOR
            .try_with(|global| global.get().map(|alloc| unsafe { alloc.alloc(layout) }))
        {
            memory
        } else {
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Ok(Some(())) = CURRNET_ALLOCATOR.try_with(|global| {
            global
                .get()
                .map(|alloc| unsafe { alloc.dealloc(ptr, layout) })
        }) {
            // done
        } else {
            unsafe { System.dealloc(ptr, layout) }
        }
    }
}

struct TackedAllocItem {
    ptr: *mut u8,
    layout: Layout,
    backtrace: Backtrace,
    during_panic: bool,
}
pub struct TrackingAlloc<T> {
    allocations: RefCell<Vec<TackedAllocItem>>,
    allocator: T,
}
impl TrackingAlloc<System> {
    pub const fn new() -> Self {
        Self::with_allocator(System)
    }
}
impl<T: GlobalAlloc> TrackingAlloc<T> {
    pub const fn with_allocator(allocator: T) -> Self {
        Self {
            allocations: RefCell::new(Vec::new()),
            allocator,
        }
    }

    pub fn forget_panic_allocations(&self) {
        self.allocations
            .borrow_mut()
            .retain(|item| !item.during_panic);
    }
    pub fn forget(&self, ptr: *mut u8) {
        let mut allocations = self.allocations.borrow_mut();
        allocations.retain(|item| !std::ptr::addr_eq(item.ptr, ptr));
    }
    pub fn forget_and_warn_all(&self) -> usize {
        let mut guard = self.allocations.borrow_mut();
        let allocations = std::mem::take(&mut *guard);
        if !allocations.is_empty() {
            eprintln!(
                "\n\n\n\nThere was {} allocations leaked with total size {} bytes\n\n\n\n",
                allocations.len(),
                allocations.iter().map(|item| item.layout.size()).sum::<usize>(),
            );
            for item in allocations.iter() {
                eprintln!(
                    "\nMemory leak with layout {:?} at:\n{}{}\n\n",
                    item.layout,
                    item.backtrace,
                    if item.during_panic {
                        " because allocated during panic"
                    } else {
                        ""
                    }
                );
            }
            eprintln!(
                "\n\nRun with RUST_BACKTRACE=1 to capture backtraces\n\
                \tNote: no backtraces will be captured for allocations during panics\n\n"
            );
            let items = allocations.len();
            drop(allocations);
            items
        } else {
            0
        }
    }
}
unsafe impl<T: GlobalAlloc> GlobalAlloc for TrackingAlloc<T> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { self.allocator.alloc(layout) };

        let Ok(mut allocations) = self.allocations.try_borrow_mut() else {
            // Allocating for Backtrace or Vec<TackedAllocItem>:
            return ptr;
        };

        let is_panicking = std::thread::panicking();
        let backtrace = if is_panicking {
            Backtrace::disabled()
        } else {
            Backtrace::capture()
        };

        allocations.push(TackedAllocItem {
            ptr,
            layout,
            backtrace,
            during_panic: is_panicking,
        });

        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Ok(mut allocations) = self.allocations.try_borrow_mut() {
            allocations.retain(|item| !std::ptr::addr_eq(item.ptr, ptr));
        }

        unsafe { self.allocator.dealloc(ptr, layout) };
    }
}
