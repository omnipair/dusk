//! Bump allocator over the largest heap frame a transaction can request.
//!
//! Blocks are placed upward from a cursor word at the start of the heap. An
//! instruction that stays within the default 32 KiB frame touches only memory
//! that frame maps, so a transaction without `ComputeBudgetProgram`'s
//! `requestHeapFrame` behaves as with the default allocator. A transaction that
//! requests a larger frame can allocate up to that frame. Memory past the frame
//! a transaction actually requested faults on first access and aborts the
//! instruction, as an exhausted default heap does.
//!
//! Nothing is ever freed, so no memory is reused. The newest block can grow in
//! place, which saves the copy and the abandoned block of a regrown vector.

#![allow(unexpected_cfgs)]

#[cfg(target_os = "solana")]
mod upward {
    use anchor_lang::solana_program::entrypoint::HEAP_START_ADDRESS;
    use core::alloc::{GlobalAlloc, Layout};

    /// `MAX_HEAP_FRAME_BYTES` of the compute-budget program.
    const MAX_HEAP_BYTES: usize = 256 * 1024;
    const CURSOR_BYTES: usize = core::mem::size_of::<usize>();

    struct UpwardBumpAllocator;

    // `Layout` caps sizes at `isize::MAX` and heap addresses stay below 2^35, so
    // none of the sums below can wrap. Wrapping arithmetic keeps the allocator
    // branch-light enough to inline at every allocation site.
    unsafe impl GlobalAlloc for UpwardBumpAllocator {
        #[inline]
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let start = HEAP_START_ADDRESS as usize;
            let cursor = start as *mut usize;
            let mut position = *cursor;
            if position == 0 {
                position = start + CURSOR_BYTES;
            }
            let mask = layout.align().wrapping_sub(1);
            let begin = position.wrapping_add(mask) & !mask;
            let end = begin.wrapping_add(layout.size());
            if end > start + MAX_HEAP_BYTES {
                return core::ptr::null_mut();
            }
            *cursor = end;
            begin as *mut u8
        }

        #[inline]
        unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}

        #[inline]
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let start = HEAP_START_ADDRESS as usize;
            let cursor = start as *mut usize;
            if *cursor == (ptr as usize).wrapping_add(layout.size()) {
                let end = (ptr as usize).wrapping_add(new_size);
                if end > start + MAX_HEAP_BYTES {
                    return core::ptr::null_mut();
                }
                // A shrink keeps the cursor: the bump allocator never reuses memory.
                *cursor = end.max(*cursor);
                return ptr;
            }
            let new_ptr = self.alloc(Layout::from_size_align_unchecked(new_size, layout.align()));
            if !new_ptr.is_null() {
                core::ptr::copy_nonoverlapping(ptr, new_ptr, core::cmp::min(layout.size(), new_size));
            }
            new_ptr
        }
    }

    #[global_allocator]
    static ALLOCATOR: UpwardBumpAllocator = UpwardBumpAllocator;
}
