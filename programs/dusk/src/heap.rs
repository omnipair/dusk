//! Bump allocator over the largest heap frame a transaction can request.
//!
//! Blocks are placed upward from a cursor word at the start of the heap. An
//! allocation that fits in the default 32 KiB is mapped by the runtime. Above
//! that, the allocator checks the last byte with a memory syscall before it
//! reports success. The heap is contiguous, so a mapped last byte guarantees
//! the whole block is mapped. Exhausting the actual frame aborts at allocation.
//!
//! Nothing is ever freed, so no memory is reused. The newest block can grow in
//! place, which saves the copy and the abandoned block of a regrown vector.

#![allow(unexpected_cfgs)]

#[cfg(target_os = "solana")]
mod upward {
    use anchor_lang::solana_program::{
        entrypoint::{HEAP_LENGTH, HEAP_START_ADDRESS},
        syscalls::sol_memset_,
    };
    use core::alloc::{GlobalAlloc, Layout};

    /// `MAX_HEAP_FRAME_BYTES` of the compute-budget program.
    const MAX_HEAP_BYTES: usize = 256 * 1024;
    const CURSOR_BYTES: usize = core::mem::size_of::<usize>();
    const DEFAULT_HEAP_END: usize = HEAP_START_ADDRESS as usize + HEAP_LENGTH;

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
            if end > DEFAULT_HEAP_END {
                // The syscall checks the VM mapping before touching a newly
                // allocated byte. No Rust reference to that address is formed.
                sol_memset_(end.wrapping_sub(1) as *mut u8, 0, 1);
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
                if end > *cursor && end > DEFAULT_HEAP_END {
                    // Validate only a new byte, never the preserved contents.
                    sol_memset_(end.wrapping_sub(1) as *mut u8, 0, 1);
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
