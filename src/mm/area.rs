//! 用户地址空间校验与跨地址空间拷贝。
//!
//! 对应原版 `include/asm/segment.h` 的 `verify_area()` / `get_user()` / `put_user()`
//! 和 `arch/x86/lib/usercopy.c` 的 `copy_from_user()` / `copy_to_user()`。
//!
//! 当前不依赖 `vm_area_struct` 链表——直接查用户页表项的 USER/RW 位。
//! 等 `mm/mmap.c` 到位后换成 VMA 遍历。

use crate::klib::errno::EFAULT;
use crate::mm::paging;

/// 校验模式：读或写。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    Read,
    Write,
}

/// 校验用户虚拟地址范围 `[vaddr, vaddr+len)` 是否可访问。
///
/// 遍历用户页表（`pml4`）确认：
/// - 地址在 user space 范围内（`>= USERSPACE_START`）
/// - 每一页都有 PTE 且 USER 位已置
/// - `AccessMode::Write` 时额外检查 RW 位
///
/// 返回 0 表示校验通过，否则返回 `-EFAULT`。
pub fn verify_area(pml4: usize, vaddr: u64, len: u64, mode: AccessMode) -> i64 {
    use paging::flags;

    if pml4 == 0 || len == 0 {
        return if len == 0 { 0 } else { -(EFAULT as i64) };
    }

    let end = vaddr.saturating_add(len);
    let mut cur = vaddr & !0xFFF; // 页对齐

    while cur < end {
        let flags = paging::get_page_flags(pml4, cur as usize);
        match flags {
            Some(f) => {
                if f & flags::PRESENT == 0 || f & flags::USER == 0 {
                    return -(EFAULT as i64);
                }
                if mode == AccessMode::Write && f & flags::RW == 0 {
                    // 可写的 COW 缺页会在这里被拦截，
                    // 但内核态的 copy_from_user 本来就不该写 COW 页——
                    // COW 突破由 page_fault handler（traps.rs）负责。
                    return -(EFAULT as i64);
                }
            }
            None => return -(EFAULT as i64),
        }
        cur += crate::mm::PAGE_SIZE as u64;
    }
    0
}

/// 遍历用户页表确认 `[vaddr, vaddr+len)` 都在恒等映射低 1GB 内。
///
/// 这是 `verify_area` 之前的旧护栏；`check_range` + `verify_area` 双检之后
/// 可以把 `verify_area` 返回 true 的路径上的 `check_range` 逐步去掉。
/// 当前阶段保留它以兼容未迁移的 syscall。
pub fn check_range(ptr: u64, len: u64) -> bool {
    const IDENTITY_LIMIT: u64 = 1 << 30;
    ptr != 0 && len <= IDENTITY_LIMIT && ptr.checked_add(len).is_some_and(|e| e <= IDENTITY_LIMIT)
}

/// 从用户空间拷贝数据到内核缓冲。
///
/// 逐页通过 `paging::translate` 找到物理地址，再用恒等映射逐字节拷贝。
/// 调用者负责 `verify_area(pml4, user_src, len, AccessMode::Read)` 已经通过。
///
/// # Safety
/// `user_src` 必须是当前进程的用户空间虚拟地址；`kern_dst` 和 `len` 是
/// 内核缓冲的有效范围。`pml4` 必须是当前进程的 PML4 物理地址。
pub unsafe fn copy_from_user(kern_dst: *mut u8, user_src: u64, len: usize, pml4: usize) -> i64 {
    if len == 0 || pml4 == 0 {
        return 0;
    }

    let mut copied = 0usize;
    while copied < len {
        let src_va = user_src.wrapping_add(copied as u64);
        let off = (src_va & 0xFFF) as usize;
        let chunk = core::cmp::min(len - copied, crate::mm::PAGE_SIZE - off);

        let phys = unsafe {
            paging::translate(pml4, src_va as usize)
        };
        let Some(phys_addr) = phys else {
            return -(EFAULT as i64);
        };

        // 恒等映射下物理地址 == 内核虚拟地址
        // SAFETY: translate 成功说明页已映射且 USER 位置位；物理地址在恒等映射内。
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_addr as *const u8,
                kern_dst.add(copied),
                chunk,
            );
        }
        copied += chunk;
    }
    len as i64
}

/// 从内核缓冲拷贝数据到用户空间。
///
/// # Safety
/// 同 [`copy_from_user`]，方向相反。
pub unsafe fn copy_to_user(user_dst: u64, kern_src: *const u8, len: usize, pml4: usize) -> i64 {
    if len == 0 || pml4 == 0 {
        return 0;
    }

    let mut copied = 0usize;
    while copied < len {
        let dst_va = user_dst.wrapping_add(copied as u64);
        let off = (dst_va & 0xFFF) as usize;
        let chunk = core::cmp::min(len - copied, crate::mm::PAGE_SIZE - off);

        let phys = unsafe {
            paging::translate(pml4, dst_va as usize)
        };
        let Some(phys_addr) = phys else {
            return -(EFAULT as i64);
        };

        // SAFETY: translate 成功说明页已映射且可写；恒等映射下物理地址 == 内核虚拟地址
        unsafe {
            core::ptr::copy_nonoverlapping(
                kern_src.add(copied),
                phys_addr as *mut u8,
                chunk,
            );
        }
        copied += chunk;
    }
    len as i64
}

/// 从用户空间拷贝一个 NUL 结尾字符串到内核缓冲（最多 `max_len-1` 字节 + NUL）。
///
/// # Safety
/// 同 [`copy_from_user`]。
pub unsafe fn strncpy_from_user(
    kern_dst: *mut u8,
    user_src: u64,
    max_len: usize,
    pml4: usize,
) -> i64 {
    if max_len == 0 || pml4 == 0 {
        return -(EFAULT as i64);
    }

    let mut i = 0usize;
    while i < max_len {
        let src_va = user_src.wrapping_add(i as u64);
        let phys = unsafe {
            paging::translate(pml4, src_va as usize)
        };
        let Some(phys_addr) = phys else {
            return -(EFAULT as i64);
        };

        // SAFETY: translate 成功，页在恒等映射内。
        let byte = unsafe { core::ptr::read_volatile(phys_addr as *const u8) };
        // SAFETY: kern_dst 由调用者保证有效。
        unsafe { core::ptr::write(kern_dst.add(i), byte) };
        if byte == 0 {
            return i as i64;
        }
        i += 1;
    }

    // 没有终止 NUL
    -(EFAULT as i64)
}
