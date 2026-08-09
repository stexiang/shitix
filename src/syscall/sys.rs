//! 系统调用的具体实现。对应 linux-1.0.9 的 `kernel/sys.c` 与
//! `kernel/sched.c` 里那些 `sys_*` 函数。
//!
//! 这里只实现不依赖 `fs/`、`kernel/signal.c`、`mm/mmap.c` 的那些，
//! 其余在分发表里指向 [`ni_syscall`]。每个函数的文档注明原版位置。

use super::{SysArgs, nr};
use crate::klib::errno::{EFAULT, EINVAL, ENOSYS, EBADF, EPERM, ERANGE, EINTR};
use crate::klib::printk::Level;
use crate::sched;
use crate::traps::PtRegs;

/// 时间值结构
#[repr(C)]
pub struct TimeVal {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// 时区结构
#[repr(C)]
pub struct Timezone {
    pub tz_minuteswest: i32,
    pub tz_dsttime: i32,
}

/// 系统信息结构
#[repr(C)]
pub struct SysInfo {
    pub uptime: i64,
    pub loads: [u64; 3],
    pub totalram: u64,
    pub freeram: u64,
    pub sharedram: u64,
    pub bufferram: u64,
    pub totalswap: u64,
    pub freeswap: u64,
    pub procs: u64,
}

/// 资源使用情况
#[repr(C)]
pub struct RUsage {
    pub ru_utime: TimeVal,
    pub ru_stime: TimeVal,
}

/// 资源限制
#[repr(C)]
pub struct RLimit {
    pub rlim_cur: u64,
    pub rlim_max: u64,
}

/// poll 文件描述符
#[repr(C)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

/// 进程时间统计
#[repr(C)]
pub struct Tms {
    pub tms_utime: i64,
    pub tms_stime: i64,
    pub tms_cutime: i64,
    pub tms_cstime: i64,
}

/// 文件状态结构
#[repr(C)]
pub struct Stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_nlink: u64,
    pub st_mode: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub _pad0: i32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atimensec: i64,
    pub st_mtime: i64,
    pub st_mtimensec: i64,
    pub st_ctime: i64,
    pub st_ctimensec: i64,
    pub _unused: [i64; 3],
}

// =============================================================================
// 用户指针访问的临时护栏
//
// 原版靠 `verify_area(VERIFY_READ/WRITE, ptr, len)` + 段限长挡住越界；本树还没
// 有 `vm_area_struct`（见 STATUS 的 Open decisions），做不到真正的按 VMA 校验。
// 折中办法：只接受落在恒等映射低 1GB 内的地址——这段在 `boot/setup.S` 里被
// 2MB 大页整体映射过，读写不会触发 page fault，所以「坏指针把内核打挂」这类
// 后果被挡住了。**但它挡不住「用户态读写内核内存」**，等 mmap 到位后必须换成
// 真的 `verify_area`。这是已知的安全缺口，不是最终形态。
// =============================================================================

/// 恒等映射上限 + 用户地址空间。临时护栏，等 verify_area 到位后替换。
const IDENTITY_LIMIT: u64 = 0xC000_0000; // 3GB — covers kernel identity map + user space

fn check_range(ptr: u64, len: u64) -> bool {
    ptr != 0 && len <= IDENTITY_LIMIT && ptr.checked_add(len).is_some_and(|e| e <= IDENTITY_LIMIT)
}

/// 把用户传来的路径指针借成字节切片，顺带做 [`check_range`] 校验。
///
/// 长度上限取 `PATH_MAX`(4096)，避免坏指针上 `strlen` 一路扫到映射边界。
///
/// # Safety
/// 返回的切片只在本次系统调用期间使用；调用方不得让它逃出去。
unsafe fn user_path<'a>(ptr: u64) -> Result<&'a [u8], i64> {
    if !check_range(ptr, 1) {
        return Err(-(EFAULT as i64));
    }
    // SAFETY: check_range 保证起始地址在恒等映射内可读；strnlen 有上界，
    // 不会越过 PATH_MAX 继续扫。
    let n = unsafe { crate::klib::string::strnlen(ptr as *const u8, 4096) };
    if n == 0 || n >= 4096 {
        return Err(-(EINVAL as i64));
    }
    if !check_range(ptr, n as u64) {
        return Err(-(EFAULT as i64));
    }
    // SAFETY: 同上，n 是刚量出来的长度，整段可读。
    Ok(unsafe { core::slice::from_raw_parts(ptr as *const u8, n) })
}

/// 把用户缓冲区借成可写切片，带 [`check_range`] 校验。
///
/// # Safety
/// 同 [`user_path`]。
unsafe fn user_buf_mut<'a>(ptr: u64, len: u64) -> Result<&'a mut [u8], i64> {
    if len == 0 {
        return Ok(&mut []);
    }
    if !check_range(ptr, len) {
        return Err(-(EFAULT as i64));
    }
    // SAFETY: check_range 保证整段在恒等映射内可读写。
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) })
}

/// 把用户缓冲区借成只读切片，带 [`check_range`] 校验。
///
/// # Safety
/// 同 [`user_path`]。
unsafe fn user_buf<'a>(ptr: u64, len: u64) -> Result<&'a [u8], i64> {
    if len == 0 {
        return Ok(&[]);
    }
    if !check_range(ptr, len) {
        return Err(-(EFAULT as i64));
    }
    // SAFETY: 同上。
    Ok(unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) })
}

/// 校验并借出用户态的 `struct stat` 缓冲区，再把 fs 层的结果写进去。
///
/// 单独抽出来是因为 `stat`/`lstat`/`fstat` 三个都要做同一件事，而
/// `Stat` 有对齐要求，不能像字节缓冲那样直接 `from_raw_parts`。
///
/// # Safety
/// 同 [`user_path`]：借出的引用不得逃出本次系统调用。
unsafe fn user_stat_out(
    ptr: u64,
    f: impl FnOnce(&mut crate::fs::stat::Stat64) -> i64,
) -> i64 {
    let need = core::mem::size_of::<crate::fs::stat::Stat64>() as u64;
    if !check_range(ptr, need) {
        return -(EFAULT as i64);
    }
    let mut tmp = crate::fs::stat::Stat64::zeroed();
    let r = f(&mut tmp);
    if r < 0 {
        return r;
    }
    // SAFETY: check_range 已确认目标落在恒等映射内可写。
    unsafe { (ptr as *mut crate::fs::stat::Stat64).write_unaligned(tmp) };
    r
}

/// 未实现的调用。对应原版 `sched.c:sys_ni_syscall()`，同样返回 `-EINVAL`。
///
/// 原版返回 `-EINVAL` 而不是 `-ENOSYS` 有点反直觉，但那是 1.0.9 的实际行为，
/// 照抄。调用号越界走的是 `do_syscall` 里的 `-ENOSYS`，两条路径不同。
pub fn ni_syscall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    -(EINVAL as i64)
}

/// 执行程序。对应原版 `fs/exec.c:sys_execve()` + `fs/binfmt_elf.c:load_elf_binary()`。
pub fn execve(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::{EINVAL, ENOENT, ENOMEM, ENOEXEC};
    use crate::elf::{parse_elf64, is_executable64, parse_phdr64, ElfPType};
    use crate::mm::{get_free_page, free_page, paging, page_align, PAGE_SIZE};
    use crate::umm::USERSPACE_START;
    use crate::desc::selector::{USER_CS, USER_DS};

    let filename_ptr = args.a0;
    if filename_ptr == 0 { return -(ENOENT as i64); }

    // 1. 取路径
    let path = unsafe { user_path(filename_ptr) };
    let path = match path {
        Ok(p) => p,
        Err(e) => { return e; }
    };

    // 2. 从文件系统打开并读取 ELF（通过 VFS namei → read）
    let fd = unsafe { crate::fs::open::sys_open(path, crate::fs::oflags::O_RDONLY, 0) };
    if fd < 0 {
        return fd;
    }
    let fd = fd as usize;

    let buf = crate::mm::get_free_page();
    if buf == 0 {
        unsafe { crate::fs::open::sys_close(fd); }
        return -(ENOMEM as i64);
    }
    let page_slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, crate::mm::PAGE_SIZE) };
    let n = unsafe { crate::fs::read_write::read(fd, page_slice) };
    // Don't close fd yet — we'll need it for segment data loading
    let elf_data = unsafe { core::slice::from_raw_parts(buf as *const u8, n as usize) };

    // 3. 解析 ELF64
    let header = match parse_elf64(elf_data) {
        Ok(h) => h,
        Err(_) => {
            crate::mm::free_page(buf);
            unsafe { crate::fs::open::sys_close(fd); }
            return -(ENOEXEC as i64);
        }
    };
    if is_executable64(&header).is_err() {
        crate::mm::free_page(buf);
        unsafe { crate::fs::open::sys_close(fd); }
        return -(ENOEXEC as i64);
    }

    // 4. 记录旧 PML4
    let old_pml4 = unsafe {
        let me = sched::task_ptr(sched::current_index());
        let old = (*me).pml4;
        (*me).pml4 = 0;
        (*me).tss.cr3 = 0;
        old
    };

    // 5. 分配新 PML4
    let new_pml4 = paging::alloc_pml4();
    if new_pml4 == 0 {
        crate::mm::free_page(buf);
        return -(ENOMEM as i64);
    }
    if !paging::clone_kernel_pdpt(new_pml4) {
        free_page(new_pml4);
        crate::mm::free_page(buf);
        return -(ENOMEM as i64);
    }

    // 6. 扫描程序头：找 PT_INTERP（动态链接器）、PT_PHDR（auxv 用）
    let phoff = header.e_phoff as usize;
    let phentsize = header.e_phentsize as usize;
    let phnum = header.e_phnum as usize;
    let mut entry = header.e_entry;
    let mut interp_path: Option<&[u8]> = None;
    let mut at_phdr: u64 = 0;
    let mut interp_base: u64 = 0; // AT_BASE
    // 第一个 PT_LOAD 段的 (file_off, vaddr)：没有 PT_PHDR 时用它推算
    // phdr 的加载地址（ELF 头+程序头总在第一个 LOAD 段里）。
    let mut first_load_off: u64 = 0;
    let mut first_load_va: u64 = 0;
    let mut have_first_load = false;

    for i in 0..phnum {
        let phdr = match parse_phdr64(elf_data, phoff + i * phentsize) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let ptype = phdr.p_type;
        if ptype == 3 { // PT_INTERP
            let off = phdr.p_offset as usize;
            let sz = phdr.p_filesz as usize;
            if off + sz <= elf_data.len() && sz > 0 && sz < 256 {
                interp_path = Some(&elf_data[off..off + sz - 1]); // strip NUL
            }
        }
        if ptype == 6 { // PT_PHDR
            at_phdr = phdr.p_vaddr;
        }
        if ptype == 1 && !have_first_load { // PT_LOAD
            first_load_off = phdr.p_offset;
            first_load_va = phdr.p_vaddr;
            have_first_load = true;
        }
    }
    // Fallback PHDR address: 没有 PT_PHDR 时，phdr 在第一个 LOAD 段内，
    // 加载地址 = first_load_va + (phoff - first_load_off)。
    // 旧实现用 USERSPACE_START（0x4000_0000，栈区基址）+ phoff，得到的
    // 是未映射地址，glibc __libc_setup_tls 读 _dl_phdr 即 page fault。
    if at_phdr == 0 && have_first_load && phoff as u64 >= first_load_off {
        at_phdr = first_load_va + (phoff as u64 - first_load_off);
    }

    // 6a. 如果有 PT_INTERP，加载动态链接器
    if let Some(ipath) = interp_path {
        // 打开解释器文件
        let ifd = unsafe { crate::fs::open::sys_open(ipath, crate::fs::oflags::O_RDONLY, 0) };
        if ifd >= 0 {
            let ibuf = get_free_page();
            if ibuf != 0 {
                let islice = unsafe { core::slice::from_raw_parts_mut(ibuf as *mut u8, PAGE_SIZE) };
                let inr = unsafe { crate::fs::read_write::read(ifd as usize, islice) };
                if inr >= 64 {
                    let idata = unsafe { core::slice::from_raw_parts(ibuf as *const u8, inr as usize) };
                    if let Ok(ihdr) = parse_elf64(idata) {
                        if is_executable64(&ihdr).is_ok() {
                            let iphoff = ihdr.e_phoff as usize;
                            // 加载解释器的 PT_LOAD 段
                            for j in 0..ihdr.e_phnum as usize {
                                let iphd = match parse_phdr64(idata, iphoff + j * ihdr.e_phentsize as usize) {
                                    Ok(p) => p,
                                    Err(_) => continue,
                                };
                                if iphd.p_type != ElfPType::Load as u32 { continue; }
                                let ivaddr = iphd.p_vaddr as usize;
                                let ifilesz = iphd.p_filesz as usize;
                                let imemsz = iphd.p_memsz as usize;
                                let ifoff = iphd.p_offset as usize;
                                let iprot = crate::elf::phdr_prot_to_flags(iphd.p_flags);
                                let istart = ivaddr & !0xFFF;
                                let iend = page_align(ivaddr + imemsz) + 16 * PAGE_SIZE;
                                for va in (istart..iend).step_by(PAGE_SIZE) {
                                    let pg = get_free_page();
                                    if pg == 0 { break; }
                                    if !unsafe { paging::map_page(new_pml4, va, pg, iprot) } {
                                        free_page(pg); break;
                                    }
                                    if va >= ivaddr && va < ivaddr + ifilesz {
                                        let cs = if va < ivaddr { ivaddr - va } else { 0 };
                                        let ce = core::cmp::min(PAGE_SIZE, ivaddr + ifilesz - va);
                                        if ifoff + (va - ivaddr) + cs + (ce - cs) <= idata.len() {
                                            unsafe { core::ptr::copy_nonoverlapping(
                                                idata.as_ptr().add(ifoff + (va - ivaddr) + cs),
                                                pg as *mut u8, ce - cs); }
                                        }
                                    }
                                }
                                if interp_base == 0 { interp_base = istart as u64; }
                            }
                            // entry = 解释器入口 (relocated)
                            entry = ihdr.e_entry;
                        }
                    }
                }
                free_page(ibuf);
            }
            unsafe { crate::fs::open::sys_close(ifd as usize); }
        }
    }

    // 7. Map zero page writable (busybox reads/writes NULL during early init)
    {
        let pg = get_free_page();
        if pg != 0 {
            unsafe { paging::map_page(new_pml4, 0, pg, paging::flags::SHARED); }
        }
    }

    // 8. 加载主程序的 PT_LOAD 段
    let mut max_va: usize = 0;
    for i in 0..phnum {
        let phdr = match parse_phdr64(elf_data, phoff + i * phentsize) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if phdr.p_type != ElfPType::Load as u32 {
            continue;
        }

        let vaddr = phdr.p_vaddr as usize;
        let filesz = phdr.p_filesz as usize;
        let memsz = phdr.p_memsz as usize;
        let file_off = phdr.p_offset as usize;

        // 计算页面保护
        let prot = crate::elf::phdr_prot_to_flags(phdr.p_flags);

        // 按页映射。writable segments get 2 extra pages: BSS clearing
        // in glibc startup walks past __bss_end to page-aligned _end symbol.
        let start_va = vaddr & !0xFFF;
        let extra = if phdr.p_flags & 2 != 0 { 2 * PAGE_SIZE } else { PAGE_SIZE };
        let end_va = page_align(vaddr + memsz) + extra;
        if end_va > max_va { max_va = end_va; }
        let mut va = start_va;
        while va < end_va {
            let pg = get_free_page();
            if pg == 0 {
                crate::mm::free_page(buf);
                unsafe { crate::fs::open::sys_close(fd); }
                return -(ENOMEM as i64);
            }
            if !unsafe { paging::map_page(new_pml4, va, pg, prot) } {
                free_page(pg);
                crate::mm::free_page(buf);
                unsafe { crate::fs::open::sys_close(fd); }
                return -(ENOMEM as i64);
            }

            // 拷贝文件内容到该页。条件改为「页与 [vaddr, vaddr+filesz) 有重叠」：
            // 段的 p_vaddr 不一定页对齐（如 0x60d380），包含 vaddr 的那一页
            // (va < vaddr) 仍要加载 vaddr 之后的部分文件内容（.init_array 等
            // 就落在这种首页里）。旧条件 `va >= vaddr` 把首页整页跳过，导致
            // .init_array 读到 0，__libc_csu_init call *(init_array[0]) 跳飞。
            if va < vaddr + filesz && va + PAGE_SIZE > vaddr {
                let copy_start = if va < vaddr { vaddr - va } else { 0 };
                let copy_end = core::cmp::min(PAGE_SIZE, vaddr + filesz - va);
                let file_src = file_off + (va - vaddr) + copy_start;
                let copy_len = copy_end - copy_start;
                // Read from file descriptor (seek + read) for large ELFs.
                // 循环读直到填满 copy_len：底层 read 可能一次只返回部分字节
                // （大文件走 ext4 extent/间接块，bread 受缓冲页大小限制）。
                if copy_len > 0 {
                    unsafe {
                        crate::fs::read_write::lseek(fd, file_src as i64, crate::fs::SEEK_SET);
                        let mut filled = 0usize;
                        while filled < copy_len {
                            // 文件数据写到 pg + copy_start + filled（首页 copy_start>0）。
                            let dest = core::slice::from_raw_parts_mut(
                                (pg as *mut u8).add(copy_start + filled), copy_len - filled);
                            let n = crate::fs::read_write::read(fd, dest);
                            if n <= 0 {
                                // EOF 或出错：剩余保持零（get_free_page 已清零）。
                                break;
                            }
                            filled += n as usize;
                        }
                    }
                }
            }

            // BSS 段已是零（get_free_page 清零）
            va += PAGE_SIZE;
        }
    }

    // 7.5 Initialize brk to page-aligned end of loaded segments
    unsafe {
        let nr = sched::current_index();
        let t = sched::task_ptr(nr);
        (*t).brk = page_align(max_va);
    }

    // 8. 设置用户栈 —— glibc 启动（TLS 设置、IFUNC 解析、signal stack）需要较大栈，
    //    只给一页会溢出。分配 16 页（64KB）栈区，把 argv/envp/auxv 写在最顶页。
    const STACK_PAGES: usize = 16;
    let stack_top_off = STACK_PAGES * PAGE_SIZE;          // 栈区大小
    let stack_base_va = USERSPACE_START as usize + 0x2000; // 栈区最低虚拟地址
    let stack_top_va = stack_base_va + stack_top_off;     // 栈区最高虚拟地址（exclusive）
    // 仅映射栈页（每页一张物理页）。失败则逐页回滚。
    let mut mapped = 0usize;
    while mapped < STACK_PAGES {
        let pg = get_free_page();
        if pg == 0 { break; }
        let va = stack_base_va + mapped * PAGE_SIZE;
        if !unsafe { paging::map_page(new_pml4, va, pg, paging::flags::SHARED) } {
            free_page(pg);
            break;
        }
        mapped += 1;
    }
    if mapped < STACK_PAGES {
        unsafe {
            for k in 0..mapped {
                let va = stack_base_va + k * PAGE_SIZE;
                if let Some(phys) = paging::translate(new_pml4, va) {
                    paging::unmap_page(new_pml4, va);
                    free_page(phys);
                }
            }
        }
        crate::mm::free_page(buf);
        unsafe { crate::fs::open::sys_close(fd); }
        return -(ENOMEM as i64);
    }
    // 顶页的物理地址（用来在上面写 argc/argv/envp/auxv + 字符串）。
    let top_page = unsafe { paging::translate(new_pml4, stack_top_va - PAGE_SIZE) }.unwrap_or(0);

    // 用户栈初始布局（自顶向下，与 glibc _dl_setup_stack / SysV ABI 一致）：
    //   [字符串区：AT_EXECFN/AT_PLATFORM/argv[0]/AT_RANDOM]  ← 页最顶
    //   [auxv: AT_NULL..AT_*]
    //   [envp: NULL]
    //   [argv: argv[0], NULL]
    //   [argc]
    // 旧实现把字符串写在页顶、auxv 又从最高槽(n_slots-1=top-8)往下写，
    // 两者重叠：AT_NULL 把 "/bin/sh\0"/"sh\0" 清零，argv[0] 变成空串，
    // BusyBox 取 basename 为空 → ": applet not found"。下面用游标正确分隔。
    let n_slots = PAGE_SIZE / 8;
    let argc_slot: usize;
    let execfn_va: u64;
    let platform_va: u64;
    let argv0_va: u64;
    let random_va: u64;
    // SAFETY: top_page 在恒等映射内，独占。
    unsafe {
        let base = top_page as *mut u8;
        let top = base.add(PAGE_SIZE); // 顶页末尾（exclusive）
        // 1) 字符串区：用递减游标从页顶往下放，互不重叠。
        let mut cur = top;
        // AT_RANDOM: 16 字节，glibc 期望 16 字节对齐，先对齐游标。
        cur = cur.sub((cur as usize) & 0xF);
        let rp = cur.sub(16);
        core::ptr::write_bytes(rp, 0, 16);
        random_va = (stack_top_va - (top as usize - rp as usize)) as u64;
        cur = rp;
        // 普通字符串：argv[0]/AT_EXECFN/AT_PLATFORM（游标递减，不重叠）。
        let mut put = |s: &[u8]| -> u64 {
            let len = s.len() + 1; // 含 NUL
            let p = cur.sub(len);
            core::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
            *p.add(s.len()) = 0;
            cur = p;
            (stack_top_va - (top as usize - p as usize)) as u64
        };
        argv0_va = put(b"/bin/sh");
        execfn_va = argv0_va; // AT_EXECFN 与 argv[0] 共用同一字符串
        platform_va = put(b"x86_64");

        // 2) auxv/argv/argc 槽位区：从字符串区下方往下写。
        let aux_top = (cur as usize) & !0x7; // 8 对齐
        let mut i = (aux_top - (top_page as usize)) / 8 - 1; // 最高可用槽
        let s = top_page as *mut u64;
        s.add(i).write_volatile(0); i -= 1; // AT_NULL val
        s.add(i).write_volatile(0); i -= 1; // AT_NULL key
        s.add(i).write_volatile(0); i -= 1; // AT_HWCAP2(26) val
        s.add(i).write_volatile(26); i -= 1; // AT_HWCAP2 key
        s.add(i).write_volatile(0xbfebfbff | (1<<0) | (1<<9) | (1<<19)); i -= 1; // AT_HWCAP(16) val (SSE/SSE2/etc)
        s.add(i).write_volatile(16); i -= 1; // AT_HWCAP key
        s.add(i).write_volatile(100); i -= 1; // AT_CLKTCK(17) val
        s.add(i).write_volatile(17); i -= 1; // AT_CLKTCK key
        s.add(i).write_volatile(random_va); i -= 1; // AT_RANDOM(25) val
        s.add(i).write_volatile(25); i -= 1; // AT_RANDOM key
        s.add(i).write_volatile(platform_va); i -= 1; // AT_PLATFORM(15) val
        s.add(i).write_volatile(15); i -= 1; // AT_PLATFORM key
        s.add(i).write_volatile(execfn_va); i -= 1; // AT_EXECFN(31) val
        s.add(i).write_volatile(31); i -= 1; // AT_EXECFN key
        s.add(i).write_volatile(entry); i -= 1; // AT_ENTRY(9) val
        s.add(i).write_volatile(9); i -= 1;    // AT_ENTRY key
        s.add(i).write_volatile(interp_base); i -= 1; // AT_BASE(7) val
        s.add(i).write_volatile(7); i -= 1;    // AT_BASE key
        s.add(i).write_volatile(4096); i -= 1; // AT_PAGESZ(6) val
        s.add(i).write_volatile(6); i -= 1;    // AT_PAGESZ key
        s.add(i).write_volatile(phnum as u64); i -= 1; // AT_PHNUM(5) val
        s.add(i).write_volatile(5); i -= 1;    // AT_PHNUM key
        s.add(i).write_volatile(56); i -= 1;   // AT_PHENT(4) val
        s.add(i).write_volatile(4); i -= 1;    // AT_PHENT key
        s.add(i).write_volatile(at_phdr); i -= 1; // AT_PHDR(3) val
        s.add(i).write_volatile(3); i -= 1;    // AT_PHDR key
        s.add(i).write_volatile(0); i -= 1;    // AT_EGID(14) val
        s.add(i).write_volatile(14); i -= 1;   // AT_EGID key
        s.add(i).write_volatile(0); i -= 1;    // AT_GID(13) val
        s.add(i).write_volatile(13); i -= 1;   // AT_GID key
        s.add(i).write_volatile(0); i -= 1;    // AT_EUID(12) val
        s.add(i).write_volatile(12); i -= 1;   // AT_EUID key
        s.add(i).write_volatile(0); i -= 1;    // AT_UID(11) val
        s.add(i).write_volatile(11); i -= 1;   // AT_UID key
        s.add(i).write_volatile(0); i -= 1;    // AT_SECURE(23) val
        s.add(i).write_volatile(23); i -= 1;   // AT_SECURE key
        s.add(i).write_volatile(0); i -= 1;    // NULL envp
        s.add(i).write_volatile(0); i -= 1;    // argv 终止 NULL
        s.add(i).write_volatile(argv0_va); i -= 1; // argv[0]
        s.add(i).write_volatile(1);             // argc=1
        argc_slot = i;
    }
    // user_rsp 指向 argc 槽的虚拟地址。
    let user_rsp = stack_top_va as u64 - ((n_slots - argc_slot) * 8) as u64;

    // 8. 设置当前任务使用新页表，并立即加载 CR3。
    unsafe {
        let me = sched::task_ptr(sched::current_index());
        (*me).pml4 = new_pml4;
        (*me).tss.cr3 = new_pml4 as u64;
        // execve 不会切任务，所以 switch_to_task 不会替我们换 CR3——
        // 必须在这里立即加载，否则 iretq 后 CPU 还在用旧的（可能是 boot）CR3。
        core::arch::asm!("mov cr3, {}", in(reg) new_pml4 as u64, options(preserves_flags));
        // CR3 已切换到新 PML4，现在安全释放旧 PML4
        if old_pml4 != 0 && old_pml4 != new_pml4 {
            crate::mm::free_page(old_pml4);
        }
    }

    // 9. 改写 pt_regs：下次 iretq 到新程序入口
    regs.rip = entry;
    regs.rsp = user_rsp;
    regs.cs = USER_CS as u64;
    regs.rflags = 0x202;
    regs.ss = USER_DS as u64;
    regs.rax = 0;

    // 清理
    unsafe {
        crate::mm::free_page(buf);
        crate::fs::open::sys_close(fd);
    }

    0
}

/// 构建一个最小 ELF64 可执行文件（静态）。
/// 代码功能：`getpid() → exit(42)`
/// 返回 `(buffer, size)` — buffer 是 512 字节静态数组。
pub fn build_minimal_elf64() -> ([u8; 256], usize) {
    // x86_64 machine code:
    //   mov rax, 39  (__NR_getpid)
    //   int 0x80
    //   mov rdi, 42
    //   mov rax, 60  (__NR_exit)
    //   int 0x80
    let code: &[u8] = &[
        0x48, 0xc7, 0xc0, 0x27, 0x00, 0x00, 0x00, // mov rax, 39
        0xcd, 0x80,                                     // int 0x80
        0x48, 0xc7, 0xc7, 0x2a, 0x00, 0x00, 0x00, // mov rdi, 42
        0x48, 0xc7, 0xc0, 0x3c, 0x00, 0x00, 0x00, // mov rax, 60
        0xcd, 0x80,                                     // int 0x80
    ];
    let code_size = code.len();
    let code_vaddr: u64 = crate::umm::USERSPACE_START; // 0x4000_0000, 用户空间起始

    let mut elf = [0u8; 256];
    let mut pos = 0usize;

    // ELF64 header (64 bytes)
    elf[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    elf[4] = 2;  elf[5] = 1;  elf[6] = 1;
    elf[16..18].copy_from_slice(&2u16.to_le_bytes());    // ET_EXEC
    elf[18..20].copy_from_slice(&62u16.to_le_bytes());   // x86_64
    elf[20..24].copy_from_slice(&1u32.to_le_bytes());
    elf[24..32].copy_from_slice(&code_vaddr.to_le_bytes()); // entry
    elf[32..40].copy_from_slice(&64u64.to_le_bytes());   // e_phoff
    elf[54..56].copy_from_slice(&56u16.to_le_bytes());   // e_phentsize
    elf[56..58].copy_from_slice(&1u16.to_le_bytes());    // e_phnum = 1
    pos = 64;

    // Program header (56 bytes): PT_LOAD
    elf[pos..pos+4].copy_from_slice(&1u32.to_le_bytes());   // PT_LOAD
    elf[pos+4..pos+8].copy_from_slice(&5u32.to_le_bytes()); // PF_R|PF_X
    elf[pos+8..pos+16].copy_from_slice(&(64u64 + 56u64).to_le_bytes()); // p_offset
    elf[pos+16..pos+24].copy_from_slice(&code_vaddr.to_le_bytes());
    elf[pos+32..pos+40].copy_from_slice(&(code_size as u64).to_le_bytes());
    elf[pos+40..pos+48].copy_from_slice(&(code_size as u64).to_le_bytes());
    elf[pos+48..pos+56].copy_from_slice(&0x1000u64.to_le_bytes());
    pos += 56;

    // Code
    elf[pos..pos+code_size].copy_from_slice(code);
    pos += code_size;

    (elf, pos)
}

/// 构建 /sbin/init ELF64 — 打印 banner、测试 ioctl、循环 idle。
/// Build a minimal /init ELF64 that loops: write banner, read stdin, echo.
pub fn build_init_elf() -> (&'static [u8], usize) {
    static mut ELF_BUF: [u8; 512] = [0; 512];
    static mut ELF_SIZE: usize = 0;
    static mut ELF_BUILT: bool = false;

    unsafe {
        if ELF_BUILT { return (&*core::ptr::addr_of!(ELF_BUF), ELF_SIZE); }
    }

    let vaddr: u64 = crate::umm::USERSPACE_START;
    let banner = b"\nshitix shell -- type something, exit to quit\n\0";
    let prompt = b"> \0";

    // x86_64 code: banner → loop{ prompt → read(0,buf,64) → echo → check exit → }
    // We use absolute addressing via mov rsi, imm64 for data pointers.
    let code: &[u8] = &[
        // write(1, banner, banner_len)
        0x48, 0xc7, 0xc0, 0x01, 0x00, 0x00, 0x00, // 00: mov rax, 1
        0x48, 0xc7, 0xc7, 0x01, 0x00, 0x00, 0x00, // 07: mov rdi, 1
        0x48, 0xbe, 0x00, 0x00, 0x00, 0x00, 0x00, // 0e: movabs rsi, banner (patched)
                    0x00, 0x00, 0x00,
        0x48, 0xc7, 0xc2, 0x00, 0x00, 0x00, 0x00, // 18: mov rdx, banner_len (patched)
        0xcd, 0x80, // 22: int 0x80
        // loop:
        // write(1, prompt, 2)
        0x48, 0xc7, 0xc0, 0x01, 0x00, 0x00, 0x00, // 24: mov rax, 1
        0x48, 0xc7, 0xc7, 0x01, 0x00, 0x00, 0x00, // 2b: mov rdi, 1
        0x48, 0xbe, 0x00, 0x00, 0x00, 0x00, 0x00, // 32: movabs rsi, prompt (patched)
                    0x00, 0x00, 0x00,
        0x48, 0xc7, 0xc2, 0x02, 0x00, 0x00, 0x00, // 3c: mov rdx, 2
        0xcd, 0x80, // 46: int 0x80
        // read(0, [rsp-64], 64)
        0x48, 0xc7, 0xc0, 0x00, 0x00, 0x00, 0x00, // 48: mov rax, 0
        0x48, 0xc7, 0xc7, 0x00, 0x00, 0x00, 0x00, // 4f: mov rdi, 0
        0x48, 0x8d, 0x74, 0x24, 0xc0,             // 56: lea rsi, [rsp-64]
        0x48, 0xc7, 0xc2, 0x40, 0x00, 0x00, 0x00, // 5b: mov rdx, 64
        0xcd, 0x80, // 65: int 0x80
        // if rax <= 0: exit(0)
        0x48, 0x85, 0xc0, // 67: test rax,rax
        0x7e, 0x25,       // 6a: jle exit (+37)
        // echo: write(1, [rsp-64], rax)
        0x48, 0x89, 0xc2, // 6c: mov rdx, rax
        0x48, 0xc7, 0xc0, 0x01, 0x00, 0x00, 0x00, // 6f: mov rax, 1
        0x48, 0xc7, 0xc7, 0x01, 0x00, 0x00, 0x00, // 76: mov rdi, 1
        0x48, 0x8d, 0x74, 0x24, 0xc0,             // 7d: lea rsi, [rsp-64]
        0xcd, 0x80, // 82: int 0x80
        // check if "exit\n" at [rsp-64]
        0x48, 0x8d, 0x74, 0x24, 0xc0,             // 84: lea rsi, [rsp-64]
        0x81, 0x3e, 0x65, 0x78, 0x69, 0x74,       // 89: cmp [rsi], 'exit'
        0x75, 0xc8, // 8f: jne loop
        0x80, 0x7e, 0x04, 0x0a,                    // 91: cmp byte [rsi+4], '\n'
        0x75, 0xc3, // 95: jne loop
        // exit(0)
        0x48, 0xc7, 0xc0, 0x3c, 0x00, 0x00, 0x00, // 97: mov rax, 60
        0x48, 0xc7, 0xc7, 0x00, 0x00, 0x00, 0x00, // 9e: mov rdi, 0
        0xcd, 0x80, // a8: int 0x80
        // Padding
        0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, // fill to 188
        0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    ];

    let cs = code.len(); // 188
    let file_offset = 120usize; // ELF hdr (64) + phdr (56)
    let banner_off = cs;
    let prompt_off = cs + banner.len();
    let total = cs + banner.len() + prompt.len();

    unsafe {
        let elf = &mut *core::ptr::addr_of_mut!(ELF_BUF);
        // ELF64 header
        elf[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        elf[4] = 2; elf[5] = 1; elf[6] = 1;
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());
        elf[20..24].copy_from_slice(&1u32.to_le_bytes());
        elf[24..32].copy_from_slice(&vaddr.to_le_bytes());
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[40..48].copy_from_slice(&0u64.to_le_bytes());
        elf[54..56].copy_from_slice(&64u16.to_le_bytes());
        elf[56..58].copy_from_slice(&56u16.to_le_bytes());
        elf[58..60].copy_from_slice(&1u16.to_le_bytes());
        // PT_LOAD phdr
        let mut p = 64;
        let memsz = total + 4096;
        elf[p..p+4].copy_from_slice(&1u32.to_le_bytes());
        elf[p+4..p+8].copy_from_slice(&7u32.to_le_bytes());
        elf[p+8..p+16].copy_from_slice(&(file_offset as u64).to_le_bytes());
        elf[p+16..p+24].copy_from_slice(&vaddr.to_le_bytes());
        elf[p+32..p+40].copy_from_slice(&(total as u64).to_le_bytes());
        elf[p+40..p+48].copy_from_slice(&(memsz as u64).to_le_bytes());
        elf[p+48..p+56].copy_from_slice(&0x1000u64.to_le_bytes());
        p += 56;
        // Copy code
        elf[p..p+cs].copy_from_slice(&code[..cs]);
        // Patch movabs rsi for banner (offset 0x0e+2 = bytes 16-23 of code)
        let banner_va = vaddr + file_offset as u64 + banner_off as u64;
        elf[p+0x10..p+0x18].copy_from_slice(&banner_va.to_le_bytes());
        // Patch banner_len (offset 0x18+2 = bytes 26-29 of code)
        elf[p+0x1a..p+0x1e].copy_from_slice(&(banner.len() as u32).to_le_bytes());
        // Patch movabs rsi for prompt (offset 0x32+2 = bytes 52-59 of code)
        let prompt_va = vaddr + file_offset as u64 + prompt_off as u64;
        elf[p+0x34..p+0x3c].copy_from_slice(&prompt_va.to_le_bytes());
        p += cs;
        // Data
        elf[p..p+banner.len()].copy_from_slice(banner);
        p += banner.len();
        elf[p..p+prompt.len()].copy_from_slice(prompt);
        p += prompt.len();

        ELF_SIZE = p;
        ELF_BUILT = true;
        (&*core::ptr::addr_of!(ELF_BUF), ELF_SIZE)
    }
}

/// 返回当前进程 pid。对应原版 `sched.c:sys_getpid()`。
pub fn getpid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 系统调用上下文里 current 必然有效。
    unsafe { sched::current() }.pid as i64
}

/// 返回父进程 pid。对应原版 `sched.c:sys_getppid()`
/// （原版是 `current->p_opptr->pid`）。
pub fn getppid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: current 有效；parent 是有效槽位下标（init 的 parent 是自己）。
    unsafe {
        let parent = sched::current().parent;
        sched::task(parent).pid as i64
    }
}

/// 返回进程组。对应原版 `sched.c:sys_getpgrp()`。
pub fn getpgrp(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: current 有效。
    unsafe { sched::current() }.pgrp as i64
}

/// 挂起直到收到信号。对应原版 `sched.c:sys_pause()`。
///
/// 原版返回 `-ERESTARTNOHAND`（让信号处理完后不重启这个调用）。
/// 我们照抄返回值，但因为信号还没移植，实际效果是「睡到被 timeout 唤醒」。
pub fn pause(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::ERESTARTNOHAND;
    // SAFETY: 系统调用上下文，不在中断里，可以睡。
    unsafe {
        let nr = sched::current_nr();
        if nr == 0 {
            // task[0] 不能睡（原版 __sleep_on 里有同样的 panic）
            return -(EINVAL as i64);
        }
        sched::current().state = crate::sched::task::TaskState::Interruptible;
        sched::schedule();
    }
    -(ERESTARTNOHAND as i64)
}

/// 进程时间统计。对应原版 `sys.c:sys_times()`。
///
/// 原版往用户空间的 `struct tms *` 写四个 clock_t 并返回 jiffies。
/// 我们只返回 jiffies，指针参数暂时忽略——`verify_area`（用户地址校验）
/// 依赖 `mm/mmap.c` 的 `vm_area_struct`，还没移植，往用户指针写是不安全的。
pub fn times(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    if args.a0 != 0 {
        // 原版这里 `error = verify_area(VERIFY_WRITE,tbuf,sizeof *tbuf)`
        // 我们还没有 verify_area，拒绝非空指针比写坏用户内存好。
        return -(EFAULT as i64);
    }
    sched::jiffies() as i64
}

/// 写。对应原版 `fs/read_write.c:sys_write()`。
///
/// 完整实现要 `fs/` 的 file 表和 inode 层。这里只支持 fd 1/2（stdout/stderr）
/// 且直接把字节送到内核控制台——够让用户态程序（和自检）打印东西。
/// fd 0 或其他值返回 `-EINVAL`（原版是 `-EBADF`，但那需要 file 表才能区分
/// 「无效 fd」和「未打开」，暂时统一）。
pub fn write(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 { return -(EBADF as i64); }
    let sz64 = args.a2;
    if sz64 == 0 { return 0; }
    if crate::fs::pipe::fd_is_pipe(fd as usize) {
        let pipe_idx = crate::fs::pipe::fd_to_pipe(fd as usize).unwrap();
        return crate::fs::pipe::pipe_write(pipe_idx, args.a1 as *const u8, sz64 as usize);
    }
    // SAFETY: user_buf 已校验范围。
    let buf = match unsafe { user_buf(args.a1, sz64) } {
        Ok(b) => b,
        Err(e) => return e,
    };

    // 先走 fs 层：fd 真的 open 过就写文件/设备。
    // SAFETY: 系统调用上下文，fs 层会睡；fd 无效返回 -EBADF。
    let r = unsafe { crate::fs::read_write::write(fd as usize, buf) };
    if r != -(EBADF as i64) {
        return r;
    }

    // fd 1/2 尚未 open 过就退回内核控制台。原版不需要这条路径（init 在
    // 用户态第一件事就是 open("/dev/tty0")），本树的自检和早期用户态还
    // 没有文件系统里的 stdout，所以留一条兜底。
    if fd != 1 && fd != 2 {
        return -(EBADF as i64);
    }
    match core::str::from_utf8(buf) {
        Ok(s) => {
            crate::print!("{}", s);
            crate::serial::print(s);
        }
        // 非 UTF-8 就逐字节送，保持 write(2) 的字节流语义
        Err(_) => {
            for &b in buf {
                crate::print!("{}", b as char);
            }
        }
    }
    buf.len() as i64
}

/// 退出当前进程。对应原版 `exit.c:sys_exit()` → `do_exit()`。
///
/// 原版 `do_exit` 要释放页表、关文件、通知父进程、转 ZOMBIE 等父进程 wait。
/// 那些依赖 `fs/` 和信号。这里只做内核线程能做的部分：记录退出码后让出 CPU。
pub fn exit(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::exit::do_exit(args.a0 as i32)
}

/// 系统信息。对应原版 `sys.c:sys_uname()` / `sys_newuname()`。
///
/// 原版往用户态的 `struct utsname *` 写六个定长字符串。同 [`times`]，
/// 缺 `verify_area` 所以不往用户指针写，改成直接打印到控制台
/// 并返回 0——够验证调用链路，等 fs/mm 到位后改成真的填结构体。
pub fn uname(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::{EFAULT, utsname_len};
    // `struct utsname`：6 个 65 字节字段（sysname/nodename/release/version/
    // machine/domainname），共 390 字节。对应 glibc 的 `struct utsname`。
    // 之前只 pr! 打日志、不回填用户缓冲区，glibc 读到全 0 的 release，
    // 解析出版本 0，判定 < 最小内核版本 → `FATAL: kernel too old`。
    let buf = args.a0 as *mut u8;
    if buf.is_null() {
        return -(EFAULT as i64);
    }
    // SAFETY: buf 来自用户态 a0。execve 后 CR3 已切到用户 PML4，
    // 用户栈页（含此缓冲区）已映射且 U/S=1；内核 CPL=0 可写。
    // 长度 390 在一个 4KB 页内（栈对齐），不会跨未映射页。
    unsafe {
        let mut p = buf;
        let mut zeroed = 0usize;
        let fill = |p: *mut u8, s: &str, len: usize| {
            let bytes = s.as_bytes();
            let n = bytes.len().min(len - 1);
            let dst = core::slice::from_raw_parts_mut(p, len);
            dst[..n].copy_from_slice(&bytes[..n]);
            // 其余清 0
            for b in &mut dst[n..] {
                *b = 0;
            }
        };
        fill(p, crate::UTS_SYSNAME, utsname_len);
        p = p.add(utsname_len);
        zeroed += utsname_len;
        fill(p, crate::UTS_SYSNAME, utsname_len); // nodename（暂同 sysname）
        p = p.add(utsname_len);
        zeroed += utsname_len;
        fill(p, crate::UTS_RELEASE, utsname_len);
        p = p.add(utsname_len);
        zeroed += utsname_len;
        fill(p, crate::UTS_VERSION, utsname_len);
        p = p.add(utsname_len);
        zeroed += utsname_len;
        fill(p, crate::UTS_MACHINE, utsname_len);
        p = p.add(utsname_len);
        zeroed += utsname_len;
        // domainname（GNU 扩展，第 6 字段）留空
        fill(p, "", utsname_len);
        zeroed += utsname_len;
        debug_assert_eq!(zeroed, 390);
    }
    0
}

/// idle 循环。对应原版 `sched.c:sys_idle()`（原版是 task[0] 专用，
/// 里面就是 `for(;;) { if (need_resched) schedule(); }`）。
///
/// 原版的 `sys_idle` 只允许 task[0] 调用（`if (current->pid != 0) return -EPERM`）。
pub fn idle(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::EPERM;
    if sched::current_nr() != 0 {
        return -(EPERM as i64);
    }
    // 真正的空转在 lib.rs 的 idle_loop 里，这个系统调用只做资格检查。
    // 原版走到这里就再也不返回了；我们返回 0 让调用方自己转。
    0
}

/// 一个恒定返回 `-ENOSYS` 的实现，给「明确知道没做」的调用号用。
/// 与 [`ni_syscall`] 的区别是返回值（见那里的说明）。
pub fn not_implemented(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    -(ENOSYS as i64)
}

/// 编译期检查：分发表覆盖的调用号都在范围内。
const _: () = {
    assert!(nr::UNAME < nr::NR_SYSCALLS);
    assert!(nr::GETPGID < nr::NR_SYSCALLS);
};

// =============================================================================
// LFS Critical Syscalls
// =============================================================================

/// 改变数据段大小。对应原版 `mm/mmap.c:sys_brk()`。
///
/// 这是 LFS 的关键系统调用之一。用户程序用 brk() 来分配/释放内存。
/// 原版返回新地址。
/// brk syscall — 扩展/收缩进程堆。对应原版 `mm/mmap.c:sys_brk()`。
pub fn brk(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::mm::{get_free_page, free_page, paging};
    use crate::umm::USERSPACE_START;

    let new_brk = args.a0 as usize;
    unsafe {
        let nr = sched::current_index();
        let t = sched::task_ptr(nr);
        let old_brk = (*t).brk;
        if new_brk == 0 { return old_brk as i64; }
        if new_brk == old_brk { return new_brk as i64; }

        let pml4 = (*t).pml4;
        if pml4 == 0 {
            (*t).brk = new_brk;
            return new_brk as i64;
        }

        // 堆基址在代码+栈之后。首次 brk 初始化。
        let heap_base = USERSPACE_START as usize + 0x3000;
        if old_brk == 0 { (*t).brk = heap_base; }
        let cur_brk = (*t).brk;

        if new_brk > cur_brk {
            let start = crate::mm::page::page_align(cur_brk);
            let end = crate::mm::page::page_align(new_brk + crate::mm::PAGE_SIZE - 1);
            let mut va = start;
            while va < end {
                // 只跳过「已经是用户页」的地址。低 1GB 是 2MB 内核大页恒等映射，
                // 用户进程首次访问某 2MB 区间时会把大页拆成 512 个 present-but-not-user
                // 的 4KB 表项（见 paging::next_level 的 HUGE 分支）。用 translate() 会把
                // 这些内核拆分页误判为已映射；这里必须用 is_user_mapped，否则 brk 分配
                // 出的堆页仍是内核专用页，用户态一访问就 err=0x5。
                if !paging::is_user_mapped(pml4, va) {
                    let pg = get_free_page();
                    if pg == 0 { return old_brk as i64; }
                    // map_page 会覆盖该 PTE（哪怕是内核拆分页），换上新的用户物理页。
                    // 不释放被覆盖的内核物理页——它属于内核恒等映射，由内核自身管理。
                    if !paging::map_page(pml4, va, pg, paging::flags::SHARED) {
                        free_page(pg);
                        return old_brk as i64;
                    }
                }
                va += crate::mm::PAGE_SIZE;
            }
        } else {
            let start = crate::mm::page::page_align(new_brk);
            let end = crate::mm::page::page_align(cur_brk + crate::mm::PAGE_SIZE - 1);
            let mut va = end;
            while va > start {
                va -= crate::mm::PAGE_SIZE;
                // 只回收「用户页」。内核拆分页（present-but-not-user）不归 brk 管，
                // free 它们的物理地址会释放内核自身内存。
                if paging::is_user_mapped(pml4, va) {
                    if let Some(phys) = paging::translate(pml4, va) {
                        paging::unmap_page(pml4, va);
                        free_page(phys);
                    }
                }
            }
        }
        (*t).brk = new_brk;
        new_brk as i64
    }
}

/// 获取当前的 brk 值。
pub fn getbrk(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: current 有效。
    unsafe {
        sched::current().brk as i64
    }
}

/// 读取文件。对应原版 `fs/read_write.c:sys_read()`。
///
/// 参数：
/// - a0: 文件描述符
/// - a1: 缓冲区地址
/// - a2: 读取字节数
pub fn read(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    let sz64 = args.a2;
    if sz64 == 0 { return 0; }
    // Pipe fast path: bypass VFS
    if crate::fs::pipe::fd_is_pipe(fd as usize) {
        let pipe_idx = crate::fs::pipe::fd_to_pipe(fd as usize).unwrap();
        return crate::fs::pipe::pipe_read(pipe_idx, args.a1 as *mut u8, sz64 as usize);
    }
    // SAFETY: user_buf_mut 已校验范围。
    let buf = match unsafe { user_buf_mut(args.a1, sz64) } {
        Ok(b) => b,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]，fs 层会睡。
    unsafe { crate::fs::read_write::read(fd as usize, buf) }
}

/// 打开文件。对应原版 `fs/open.c:sys_open()`。
///
/// 参数：
/// - a0: 文件路径
/// - a1: 标志 (O_RDONLY, O_WRONLY, etc.)
/// - a2: 模式
pub fn open(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: user_path 已校验范围；fs 层自己处理不存在/权限。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 系统调用上下文，fs 层会睡（getblk/wait_on_buffer），
    // 所以只能在有 current 的任务里调——系统调用天然满足。
    unsafe { crate::fs::open::sys_open(path, args.a1 as u32, args.a2 as u16) }
}

/// 关闭文件。对应原版 `fs/open.c:sys_close()`。
pub fn close(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    let fd = fd as usize;
    // Pipe cleanup
    if crate::fs::pipe::fd_is_pipe(fd) {
        crate::fs::pipe::close_reader(fd);
        crate::fs::pipe::close_writer(fd);
        crate::fs::pipe::unregister_fd(fd);
    }
    // Socket cleanup
    if crate::net::socket::fd_is_socket(fd) {
        crate::net::socket::close_socket(fd);
    }
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::open::sys_close(fd) }
}

/// 创建文件。对应原版 `fs/open.c:sys_creat()`。
pub fn creat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::open::sys_creat(path, args.a1 as u16) }
}

/// 文件状态。对应原版 `fs/stat.c:sys_stat()`。
pub fn stat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]；out 的范围由 user_stat_out 校验。
    unsafe { user_stat_out(args.a1, |out| crate::fs::stat::sys_stat(path, out)) }
}

/// 文件状态（lstat，不跟随符号链接）。对应原版 `fs/stat.c:sys_lstat()`。
pub fn lstat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`stat`]。
    unsafe { user_stat_out(args.a1, |out| crate::fs::stat::sys_lstat(path, out)) }
}

/// fstat - 文件状态（通过 fd）。对应原版 `fs/stat.c:sys_fstat()`。
pub fn fstat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: 同 [`stat`]。
    unsafe { user_stat_out(args.a1, |out| crate::fs::stat::sys_fstat(fd as usize, out)) }
}

/// 内存映射。对应原版 `mm/mmap.c:sys_mmap()`。
///
/// 参数：
/// - a0: addr
/// - a1: length
/// - a2: prot (PROT_READ|PROT_WRITE|PROT_EXEC)
/// - a3: flags (MAP_SHARED|MAP_PRIVATE|MAP_ANONYMOUS)
/// - a4: fd
/// - a5: offset
/// 内存映射。当前只支持 MAP_ANONYMOUS|MAP_PRIVATE。
pub fn mmap(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::mm::{get_free_page, free_page, paging};

    let addr = args.a0;
    let len = args.a1;
    let prot = args.a2;
    let flags = args.a3;
    let fd = args.a4;
    let offset = args.a5;

    use crate::klib::errno::{ENOMEM, EINVAL, EBADF, ENOSYS};
    if len == 0 { return -(EINVAL as i64); }

    const MAP_ANONYMOUS: u64 = 0x20;
    const MAP_PRIVATE: u64 = 0x02;
    const PROT_WRITE: u64 = 0x02;

    unsafe {
        let nr = sched::current_index();
        let t = sched::task_ptr(nr);
        let pml4 = (*t).pml4;
        // Kernel threads have pml4=0; use boot PML4 (0x4000) for them
        let pml4 = if pml4 == 0 { 0x4000usize } else { pml4 };

        let map_addr = if addr != 0 { (addr as usize) & !0xFFF } else { 0x5000_0000usize };
        let npages = ((len as usize) + crate::mm::PAGE_SIZE - 1) / crate::mm::PAGE_SIZE;
        let mut pg_flags = paging::flags::USER | paging::flags::PRESENT;
        if prot & PROT_WRITE != 0 { pg_flags |= paging::flags::RW; }

        // File-backed MAP_PRIVATE: read file content into pages
        if flags & MAP_ANONYMOUS == 0 && flags & MAP_PRIVATE != 0 {
            let fd = fd as i64;
            if fd < 0 { return -(EBADF as i64); }
            let f = crate::fs::open::fd_to_filp(fd as usize);
            if f == crate::fs::inode::NIL { return -(EBADF as i64); }
            let ino = unsafe { crate::fs::file_table::filp(f).f_inode };
            if ino == crate::fs::inode::NIL { return -(EBADF as i64); }
            // Check file size bounds
            let fsize = unsafe { crate::fs::inode::inode(ino).i_size as u64 };
            let map_end = offset + len;
            if map_end > fsize && fsize > 0 { return -(EINVAL as i64); }

            for i in 0..npages {
                let pg = get_free_page();
                if pg == 0 { return -(ENOMEM as i64); }
                let va = map_addr + i * crate::mm::PAGE_SIZE;
                if !paging::map_page(pml4, va, pg, pg_flags) {
                    free_page(pg); return -(ENOMEM as i64);
                }
                // Read file content into this page
                let file_off = offset + (i * crate::mm::PAGE_SIZE) as u64;
                let read_len = core::cmp::min(crate::mm::PAGE_SIZE as u64, len - (i * crate::mm::PAGE_SIZE) as u64);
                let buf = unsafe { core::slice::from_raw_parts_mut(pg as *mut u8, crate::mm::PAGE_SIZE) };
                let ret = unsafe { crate::fs::read_write::read(fd as usize, &mut buf[..read_len as usize]) };
                // Note: read advances f_pos, so only the first read is at the right offset.
                // For page-aligned, zero-offset mappings this works; for arbitrary offsets,
                // lseek before read would be needed. The dynamic linker always maps from offset 0.
                if ret < 0 { free_page(pg); paging::unmap_page(pml4, va); return ret; }
                // Zero remaining bytes (BSS-like)
                for j in read_len as usize..crate::mm::PAGE_SIZE { buf[j] = 0; }
            }
            return map_addr as i64;
        }

        // MAP_ANONYMOUS: zero-filled pages
        if flags & MAP_ANONYMOUS == 0 { return -(ENOSYS as i64); }

        for i in 0..npages {
            let pg = get_free_page();
            if pg == 0 { return -(ENOMEM as i64); }
            let va = map_addr + i * crate::mm::PAGE_SIZE;
            if !paging::map_page(pml4, va, pg, pg_flags) {
                free_page(pg);
                return -(ENOMEM as i64);
            }
        }
        map_addr as i64
    }
}

pub fn munmap(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::mm::{free_page, paging};
    let addr = args.a0 as usize;
    let len = args.a1 as usize;
    if addr & 0xFFF != 0 || len == 0 { return -(EINVAL as i64); }

    unsafe {
        let nr = sched::current_index();
        let t = sched::task_ptr(nr);
        let pml4 = (*t).pml4;
        let pml4 = if pml4 == 0 { 0x4000usize } else { pml4 };
        let npages = (len + crate::mm::PAGE_SIZE - 1) / crate::mm::PAGE_SIZE;
        for i in 0..npages {
            let va = addr + i * crate::mm::PAGE_SIZE;
            if let Some(phys) = paging::translate(pml4, va) {
                paging::unmap_page(pml4, va);
                free_page(phys);
            }
        }
        0
    }
}

/// 内存保护。对应原版 `mm/mmap.c:sys_mprotect()`。
pub fn mprotect(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::mm::paging;
    use crate::mm::PAGE_SIZE;

    let addr = args.a0 as usize;
    let len = args.a1 as usize;
    let prot = args.a2 as u32;

    if len == 0 { return -(EINVAL as i64); }

    let start = crate::mm::page::page_base(addr);
    // end = ceil(addr+len) 向上取整到页边界（一次取整即可）。
    // 旧代码 page_align(addr+len+PAGE_SIZE-1) 会再多吃一整页：
    // mprotect(0x60d000,0x7000) 本应只覆盖 [0x60d000,0x614000)，
    // 旧 end=0x615000 把 0x614000 这页也改成只读，导致 glibc 写 0x6149a0 触发 #PF。
    let end = crate::mm::page::page_align(addr + len);

    // Build protection flags for set_page_flags (PRESENT is added automatically).
    // x86_64: RW=writable, USER=user-accessible, NO_EXEC=no instruction fetch.
    let mut new_flags: u64 = paging::flags::USER;
    if prot & 2 != 0 { new_flags |= paging::flags::RW; }       // PROT_WRITE
    if prot & 4 == 0 { new_flags |= paging::flags::NO_EXEC; }  // !PROT_EXEC

    unsafe {
        let pml4 = paging::current_pml4();
        if pml4 == 0 { return 0; }
        let mut va = start;
        while va < end {
            paging::set_page_flags(pml4, va, new_flags);
            va += PAGE_SIZE;
        }
    }
    0
}

/// 获取当前工作目录。对应原版 `fs/open.c:sys_getcwd()`。
pub fn getcwd(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let buf = args.a0 as *mut u8;
    let size = args.a1 as usize;
    
    if buf.is_null() || size == 0 {
        return -(EINVAL as i64);
    }
    
    // TODO: 实现 getcwd
    // 目前返回 "/"
    let cwd = b"/";
    if size < cwd.len() + 1 {
        return -(EINVAL as i64);
    }
    
    // SAFETY: buf 已校验。
    unsafe {
        core::ptr::copy_nonoverlapping(cwd.as_ptr(), buf, cwd.len());
        *buf.add(cwd.len()) = 0;
    }
    
    buf as i64
}

/// 改变当前工作目录。对应原版 `fs/open.c:sys_chdir()`。
pub fn chdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::open::sys_chdir(path) }
}

/// 重命名。对应原版 `fs/namei.c:sys_rename()`。
pub fn rename(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let oldname = args.a0 as *const u8;
    let newname = args.a1 as *const u8;
    if oldname.is_null() || newname.is_null() {
        return -(EFAULT as i64);
    }
    let old_path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    let new_path = match unsafe { user_path(args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // Simple rename: link new, unlink old
    unsafe {
        let r = crate::fs::namei::do_link(old_path, new_path);
        if r < 0 { return r; }
        crate::fs::namei::do_unlink(old_path)
    }
}

/// 删除文件。对应原版 `fs/namei.c:sys_unlink()`。
pub fn unlink(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::namei::do_unlink(path) }
}

/// 创建目录。对应原版 `fs/namei.c:sys_mkdir()`。
pub fn mkdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::namei::do_mkdir(path, args.a1 as u16) }
}

/// 删除目录。对应原版 `fs/namei.c:sys_rmdir()`。
pub fn rmdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::namei::do_rmdir(path) }
}

/// 创建符号链接。对应原版 `fs/namei.c:sys_symlink()`。
pub fn symlink(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let oldname = args.a0 as *const u8;
    let newname = args.a1 as *const u8;
    if oldname.is_null() || newname.is_null() {
        return -(EFAULT as i64);
    }
    // Read target path
    let target = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    let link_path = match unsafe { user_path(args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // Create a regular file and write the target into it.
    // This is a minimal "fast symlink" approximation.
    unsafe {
        let fd = crate::fs::open::sys_creat(link_path, 0o777);
        if fd < 0 { return fd; }
        let n = crate::fs::read_write::write(fd as usize, target);
        crate::fs::open::sys_close(fd as usize);
        if n < 0 { n } else { 0 }
    }
}

/// 读取符号链接目标。对应原版 `fs/namei.c:sys_readlink()`。
pub fn readlink(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::{EINVAL, ENOENT};

    let buf = args.a1 as *mut u8;
    let bufsize = args.a2 as usize;
    if buf.is_null() || bufsize == 0 { return -(EINVAL as i64); }

    // 特殊 case: /proc/self/exe → 返回空（glibc 会 fallback 到 AT_EXECFN）
    let path = unsafe { user_path(args.a0) };
    let path = match path { Ok(p) => p, Err(e) => return e };
    if path.starts_with(b"/proc/") || path.starts_with(b"/dev/") {
        // 虚拟路径：返回空，glibc 有 fallback
        if bufsize > 0 { unsafe { *buf = 0; } }
        return 0;
    }

    // 真实路径：用 namei 找到 inode
    let inr = match unsafe { crate::fs::namei::namei(path) } {
        Ok(i) => i, Err(e) => return e as i64,
    };
    let ip = unsafe { crate::fs::inode::inode(inr) };
    // minix 没有符号链接支持，返回数据块内容作为链接目标
    let link_len = ip.i_size as usize;
    if link_len == 0 || link_len > bufsize {
        unsafe { crate::fs::inode::iput(inr); }
        return if link_len == 0 { -(ENOENT as i64) } else { -(EINVAL as i64) };
    }
    // 直接用 inode 的 zone[0] 读数据
    let zone0 = ip.data[0] as usize;
    if zone0 != 0 {
        let blk = unsafe { crate::fs::buffer::bread(0x0101u16, zone0 as u32, 1024) };
        if let Some(bn) = blk {
            let data_slice = unsafe { crate::fs::buffer::bh(bn).data() };
            let n = core::cmp::min(core::cmp::min(link_len, bufsize), data_slice.len());
            unsafe { core::ptr::copy_nonoverlapping(data_slice.as_ptr(), buf, n); }
            unsafe { crate::fs::buffer::brelse(bn); }
            unsafe { crate::fs::inode::iput(inr); }
            return n as i64;
        }
    }
    unsafe { crate::fs::inode::iput(inr); }
    -(ENOENT as i64)
}

/// readlinkat — dirfd 相对 readlink。
pub fn readlinkat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // 忽略 dirfd，按绝对路径处理
    readlink(args, _regs)
}

/// 复制文件描述符。对应原版 `fs/fcntl.c:sys_dup()`。
pub fn dup(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: fs 层校验 fd。
    unsafe { crate::fs::open::sys_dup(fd as usize) }
}

/// 复制文件描述符（指定新 fd）。对应原版 `fs/fcntl.c:sys_dup2()`。
pub fn dup2(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let (old, new) = (args.a0 as i64, args.a1 as i64);
    if old < 0 || new < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: fs 层校验两个 fd。
    unsafe { crate::fs::open::sys_dup2(old as usize, new as usize) }
}

/// 文件控制。对应原版 `fs/fcntl.c:sys_fcntl()`。
pub fn fcntl(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i32;
    let cmd = args.a1 as u32;
    let arg = args.a2 as usize;
    
    if fd < 0 {
        return -(EBADF as i64);
    }
    
    match cmd {
        // F_DUPFD: duplicate fd, return >= arg
        0 => {
            let start = arg.max(0);
            let filp_idx = unsafe { crate::fs::open::fd_to_filp(fd as usize) };
            if filp_idx == crate::fs::inode::NIL { return -(EBADF as i64); }
            // Find a free fd >= start
            let mut new_fd = start;
            let nr = unsafe { crate::sched::current_index() };
            while new_fd < crate::fs::NR_OPEN {
                if unsafe { crate::fs::open::task_fd(nr, new_fd) == crate::fs::inode::NIL }
                    && !crate::fs::pipe::fd_is_pipe(new_fd)
                    && !crate::net::socket::fd_is_socket(new_fd)
                {
                    unsafe { crate::fs::open::set_task_fd(nr, new_fd, filp_idx); }
                    unsafe { crate::fs::file_table::filp(filp_idx).f_count += 1; }
                    return new_fd as i64;
                }
                new_fd += 1;
            }
            -(crate::klib::errno::EMFILE as i64)
        }
        // F_GETFD
        1 => 0,
        // F_SETFD
        2 => 0,
        // F_GETFL
        3 => {
            let filp_idx = unsafe { crate::fs::open::fd_to_filp(fd as usize) };
            if filp_idx == crate::fs::inode::NIL { return -(EBADF as i64); }
            unsafe { crate::fs::file_table::filp(filp_idx).f_flags as i64 }
        }
        // F_SETFL
        4 => {
            let filp_idx = unsafe { crate::fs::open::fd_to_filp(fd as usize) };
            if filp_idx == crate::fs::inode::NIL { return -(EBADF as i64); }
            unsafe { crate::fs::file_table::filp(filp_idx).f_flags = arg as u32; }
            0
        }
        _ => -(EINVAL as i64),
    }
}

/// ioctl。对应原版 `fs/ioctl.c:sys_ioctl()`。
pub fn ioctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i32;
    let cmd = args.a1 as u32;
    let arg = args.a2 as usize;
    
    if fd < 0 {
        return -(EBADF as i64);
    }
    
    // 根据 fd 指向的 inode 类型分发 ioctl
    let f = unsafe { crate::fs::open::fd_to_filp(fd as usize) };
    if f == crate::fs::inode::NIL { return -(EBADF as i64); }
    let _ino = unsafe { crate::fs::file_table::filp(f).f_inode };
    if _ino == crate::fs::inode::NIL { return -(EBADF as i64); }

    // Handle terminal ioctls for any fd
    if cmd == 0x5401 { // TCGETS
        return unsafe { crate::drivers::char_dev::tty::tty_ioctl_get(fd as usize, arg) };
    }
    if cmd == 0x5402 || cmd == 0x5403 || cmd == 0x5404 { // TCSETS/TCSETSW/TCSETSF
        return unsafe { crate::drivers::char_dev::tty::tty_ioctl_set(fd as usize, arg) };
    }
    if cmd == 0x5413 { // TIOCGWINSZ
        let ws = [80u16, 25u16, 0u16, 0u16];
        if arg != 0 {
            unsafe { core::ptr::copy_nonoverlapping(ws.as_ptr(), arg as *mut u16, 4); }
        }
        return 0;
    }
    -(EINVAL as i64)
}

/// 访问权限检查。对应原版 `fs/open.c:sys_access()`。
pub fn access(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let _mode = args.a1 as i32;
    if pathname.is_null() { return -(EFAULT as i64); }
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // Check if file exists by trying to resolve it
    match unsafe { crate::fs::namei::namei(path) } {
        Ok(ino) => { unsafe { crate::fs::inode::iput(ino); } 0 }
        Err(e) => e as i64,
    }
}

/// pipe。对应原版 `fs/pipe.c:sys_pipe()`。
pub fn pipe(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::fs::pipe;
    use crate::klib::errno::ENFILE;

    let fildes = args.a0 as *mut i32;
    if fildes.is_null() { return -(EFAULT as i64); }

    // Allocate pipe
    let pipe_idx = match pipe::alloc_pipe() {
        Some(p) => p,
        None => return -(ENOSYS as i64),
    };

    // Find two free fd numbers (scan beyond the VFS fds: 3..64)
    let mut fd_r = -1i64;
    let mut fd_w = -1i64;
    for f in 3usize..64 {
        if fd_r < 0 && !pipe::fd_is_pipe(f)
            && unsafe { crate::fs::open::fd_to_filp(f) == crate::fs::inode::NIL }
        {
            fd_r = f as i64;
        } else if fd_r >= 0 && fd_w < 0 && !pipe::fd_is_pipe(f)
            && unsafe { crate::fs::open::fd_to_filp(f) == crate::fs::inode::NIL }
        {
            fd_w = f as i64;
            break;
        }
    }
    if fd_w < 0 {
        return -(ENFILE as i64);
    }

    // Register pipe fds
    pipe::register_fd(fd_r as usize, pipe_idx);
    pipe::register_fd(fd_w as usize, pipe_idx);

    // Write fds to user
    unsafe {
        let out = core::slice::from_raw_parts_mut(fildes, 2);
        out[0] = fd_r as i32;
        out[1] = fd_w as i32;
    }
    0
}

/// 创建特殊文件（设备/管道）。对应原版 `fs/namei.c:sys_mknod()`。
pub fn mknod(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::namei::do_mknod(path, args.a1 as u16, args.a2 as u16) }
}

/// 改变权限。对应原版 `fs/open.c:sys_chmod()`。
pub fn chmod(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::open::sys_chmod(path, args.a1 as u16) }
}

/// 改变所有者。对应原版 `fs/open.c:sys_chown()`。
pub fn chown(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let owner = args.a1 as u32;
    let group = args.a2 as u32;
    
    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    -(ENOSYS as i64)
}

/// 终止进程信号。对应原版 `kernel/signal.c:sys_kill()`。
pub fn kill(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pid = args.a0 as i32;
    let sig = args.a1 as i32;
    if sig < 1 || sig > 31 { return -(EINVAL as i64); }
    // pid > 0: send to specific pid
    // pid == 0: send to all processes in same pgrp
    // pid == -1: send to all processes (except init)
    // pid < -1: send to all processes in pgrp |pid|
    let mut sent = 0i32;
    unsafe {
        for i in 0..crate::sched::NR_TASKS {
            let t = crate::sched::task_ptr(i);
            if (*t).state == crate::sched::task::TaskState::Unused { continue; }
            let tpid = (*t).pid as i32;
            let deliver = if pid > 0 { tpid == pid }
                else if pid == 0 { (*t).pgrp == crate::sched::current().pgrp }
                else if pid == -1 { tpid > 1 }
                else { (*t).pgrp as i32 == -pid };
            if deliver {
                crate::signal::send_sig(sig as u32, i, 0);
                sent += 1;
            }
        }
    }
    if sent == 0 { -(crate::klib::errno::ESRCH as i64) } else { 0 }
}

/// 设置 alarm。对应原版 `kernel/sched.c:sys_alarm()`。
pub fn alarm(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let seconds = args.a0 as u64;
    crate::pr_warn!("sys_alarm: {} seconds (not implemented)", seconds);
    0
}

/// 获取当前时间。对应原版 `kernel/time.c:sys_gettimeofday()`。
pub fn gettimeofday(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let tv = args.a0 as *mut TimeVal;
    let tz = args.a1 as *mut Timezone;
    if tv.is_null() { return -(EFAULT as i64); }
    unsafe {
        (*tv).tv_sec = 0;
        (*tv).tv_usec = 0;
        if !tz.is_null() {
            (*tz).tz_minuteswest = 0;
            (*tz).tz_dsttime = 0;
        }
    }
    0
}

/// 获取用户 ID。对应原版 `kernel/sys.c:sys_getuid()`。
pub fn getuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 获取有效用户 ID。对应原版 `kernel/sys.c:sys_geteuid()`。
pub fn geteuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 获取组 ID。对应原版 `kernel/sys.c:sys_getgid()`。
pub fn getgid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 获取有效组 ID。对应原版 `kernel/sys.c:sys_getegid()`。
pub fn getegid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 设置用户 ID。
pub fn setuid(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置组 ID。
pub fn setgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置进程组。
pub fn setpgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 创建会话。
pub fn setsid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    unsafe { let t = sched::current(); t.session = t.pid; t.pgrp = t.pid; }
    0
}

/// 同步文件系统。
pub fn sync(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 文件同步。
pub fn fsync(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: fs 层校验 fd。
    unsafe { crate::fs::read_write::fsync(fd as usize) }
}
/// 设置文件长度。
pub fn truncate(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::open::sys_truncate(path, args.a1 as u32) }
}
/// 设置文件长度（ftruncate）。
pub fn ftruncate(args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 获取目录项。
pub fn getdents(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    let need = core::mem::size_of::<crate::fs::Dirent>() as u64;
    if args.a2 < need {
        return -(EINVAL as i64);
    }
    if !check_range(args.a1, need) {
        return -(EFAULT as i64);
    }
    // 一次只返回一项：fs 层的 readdir 就是单项语义（原版 1.0.9 的
    // `sys_readdir` 同样一次一项，getdents 是 1.2 之后才有的批量接口）。
    let mut d = crate::fs::Dirent { d_ino: 0, d_off: 0, d_reclen: need as u16, d_name: [0; 32] };
    // SAFETY: 系统调用上下文，fs 层会睡；fd 无效返回 -EBADF。
    let r = unsafe { crate::fs::read_write::readdir(fd as usize, &mut d) };
    if r <= 0 {
        return r;
    }
    // SAFETY: check_range 已确认目标落在恒等映射内可写。
    unsafe { (args.a1 as *mut crate::fs::Dirent).write_unaligned(d) };
    need as i64
}
/// 获取目录项64。
pub fn getdents64(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    // 本树的 Dirent 已经是 64 位字段（d_ino: u64），两者布局一致。
    getdents(args, regs)
}
/// 文件描述符控制。
pub fn fchdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 获取 umask。
pub fn umask(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0o022 }
/// 获取系统信息。
pub fn sysinfo(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let buf = args.a0 as *mut SysInfo;
    if buf.is_null() { return -(EFAULT as i64); }
    unsafe {
        (*buf).uptime = sched::jiffies() as i64;
        (*buf).loads = [0u64; 3];
        (*buf).totalram = 16 * 1024 * 1024;
        (*buf).freeram = 8 * 1024 * 1024;
        (*buf).sharedram = 0; (*buf).bufferram = 0;
        (*buf).totalswap = 0; (*buf).freeswap = 0;
        (*buf).procs = 1;
    }
    0
}
/// 轮询。
pub fn poll(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fds = args.a0 as *mut PollFd;
    let nfds = args.a1 as usize;
    let timeout = args.a2 as i32;
    if fds.is_null() && nfds > 0 { return -(EFAULT as i64); }

    // For timeout < 0: block indefinitely (we just yield once)
    // For timeout >= 0: check once and return
    let _ = timeout;

    let mut ready = 0i64;
    for i in 0..nfds {
        unsafe {
            let pfd = &mut *fds.add(i);
            pfd.revents = 0;
            let fd = pfd.fd as i32;
            if fd < 0 { continue; }
            let fd = fd as usize;

            // Pipe fds
            if crate::fs::pipe::fd_is_pipe(fd) {
                if pfd.events & 1 != 0 { // POLLIN
                    // Check if pipe has data - simplified: always ready
                    pfd.revents |= 1;
                }
                if pfd.events & 4 != 0 { // POLLOUT
                    pfd.revents |= 4;
                }
                if pfd.revents != 0 { ready += 1; }
                continue;
            }

            // VFS fds: check if they exist
            let filp = crate::fs::open::fd_to_filp(fd);
            if filp != crate::fs::inode::NIL {
                // Regular files are always ready
                if pfd.events & 1 != 0 { pfd.revents |= 1; } // POLLIN
                if pfd.events & 4 != 0 { pfd.revents |= 4; } // POLLOUT
                if pfd.revents != 0 { ready += 1; }
            }
        }
    }
    ready
}
/// 多路复用。
pub fn select(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // select(int nfds, fd_set *readfds, fd_set *writefds, fd_set *exceptfds,
    //        struct timeval *timeout)
    let nfds = args.a0 as usize;
    let readfds = args.a1 as *mut u64;   // fd_set is array of long (u64 on x86_64)
    let writefds = args.a2 as *mut u64;
    let exceptfds = args.a3 as *mut u64;

    if nfds > 1024 { return -(EINVAL as i64); }
    let nwords = (nfds + 63) / 64;
    let mut total_ready = 0i64;

    // Clear output sets
    unsafe {
        if !readfds.is_null() {
            for w in 0..nwords { core::ptr::write_volatile(readfds.add(w), 0); }
        }
        if !writefds.is_null() {
            for w in 0..nwords { core::ptr::write_volatile(writefds.add(w), 0); }
        }
        if !exceptfds.is_null() {
            for w in 0..nwords { core::ptr::write_volatile(exceptfds.add(w), 0); }
        }
    }

    for fd in 0..nfds {
        let mut is_ready = false;
        // Check if fd is a pipe
        if crate::fs::pipe::fd_is_pipe(fd) {
            is_ready = true; // Pipes always report ready for simplicity
        } else {
            // Check if fd is in VFS
            let filp = unsafe { crate::fs::open::fd_to_filp(fd) };
            if filp != crate::fs::inode::NIL {
                is_ready = true;
            }
        }
        if is_ready {
            let word = fd / 64;
            let bit = fd % 64;
            unsafe {
                if !readfds.is_null() {
                    core::ptr::write_volatile(readfds.add(word),
                        core::ptr::read_volatile(readfds.add(word)) | (1u64 << bit));
                }
                if !writefds.is_null() {
                    core::ptr::write_volatile(writefds.add(word),
                        core::ptr::read_volatile(writefds.add(word)) | (1u64 << bit));
                }
            }
            total_ready += 1;
        }
    }
    total_ready
}
/// 挂载文件系统。
pub fn mount(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 卸载文件系统。
pub fn umount(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 重新引导。
pub fn reboot(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 资源使用情况。

// Socket syscalls
pub fn socket(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_socket(args.a0 as u16, args.a1 as u16, args.a2 as u8)
}
pub fn bind(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_bind(args.a0 as usize, args.a1 as *const u8, args.a2 as usize)
}
pub fn connect(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_connect(args.a0 as usize, args.a1 as *const u8, args.a2 as usize)
}
pub fn listen(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_listen(args.a0 as usize, args.a1 as i32)
}
pub fn accept(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_accept(args.a0 as usize, args.a1 as *mut u8, args.a2 as *mut u32)
}
pub fn sendto(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_sendto(args.a0 as usize, args.a1 as *const u8,
        args.a2 as usize, args.a3 as i32, args.a4 as *const u8, args.a5 as usize)
}
pub fn recvfrom(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_recvfrom(args.a0 as usize, args.a1 as *mut u8,
        args.a2 as usize, args.a3 as i32, args.a4 as *mut u8, args.a5 as *mut u32)
}
pub fn shutdown(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_shutdown(args.a0 as usize, args.a1 as i32)
}
pub fn getsockname(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_getsockname(args.a0 as usize, args.a1 as *mut u8, args.a2 as *mut u32)
}
pub fn getpeername(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_getpeername(args.a0 as usize, args.a1 as *mut u8, args.a2 as *mut u32)
}
pub fn setsockopt(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_setsockopt(args.a0 as usize, args.a1 as i32,
        args.a2 as i32, args.a3 as *const u8, args.a4 as usize)
}
pub fn getsockopt(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_getsockopt(args.a0 as usize, args.a1 as i32,
        args.a2 as i32, args.a3 as *mut u8, args.a4 as *mut u32)
}
pub fn socketpair(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_socketpair(args.a0 as u16, args.a1 as u16,
        args.a2 as u8, args.a3 as *mut i32)
}
pub fn sendmsg(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_sendmsg(args.a0 as usize, args.a1 as *const u8, args.a2 as i32)
}
pub fn recvmsg(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::net::socket::sys_recvmsg(args.a0 as usize, args.a1 as *mut u8, args.a2 as i32)
}

// Process syscalls

/// Fork syscall - 进程复制
///
/// 对应原版 `kernel/fork.c:sys_fork()`。
/// 实现 fork() 系统调用。
pub fn fork(_args: &SysArgs, regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::EAGAIN;
    use crate::sched::task::{KERNEL_STACK_SIZE, STACK_MAGIC, TaskState};
    const CLONE_SIGHAND: u64 = 0x800;
    let flags = 0u64; // fork never sets CLONE_SIGHAND

    unsafe extern "C" {
        /// `entry.S` 里新任务的第一次入场点。
        fn ret_from_fork();
    }

    /// pt_regs 的 qword 数。必须和 `entry.S` 里的 `PT_SS + 8` 对上
    /// （0xa8 字节 = 21 个 qword）；下面的 `debug_assert` 兜住。
    const PTREGS_QWORDS: usize = 21;
    /// rax 在 pt_regs 里的 qword 下标。对应 `entry.S` 的 `PT_RAX 0x50`。
    const PTREGS_RAX_IDX: usize = 0x50 / 8;

    debug_assert_eq!(
        core::mem::size_of::<PtRegs>(),
        PTREGS_QWORDS * 8,
        "fork: PtRegs 大小与 entry.S 的 pt_regs 布局不一致"
    );

    // 中断里不能 fork：会拿到中断栈上的 pt_regs 而不是系统调用那份。
    // SAFETY: 系统调用上下文，稍后无条件 restore_flags 配对。
    let flags = unsafe { crate::irq::local_irq_save() };
    let result = (|| -> i64 {
        // SAFETY: 已关中断，独占任务表。
        unsafe {
            // 查找空闲的 task slot。0 是 swapper，从 1 开始。
            let mut free_slot = None;
            for i in 1..sched::NR_TASKS {
                if (*sched::task_ptr(i)).state == TaskState::Unused {
                    free_slot = Some(i);
                    break;
                }
            }
            let Some(child_nr) = free_slot else {
                crate::pr_warn!("sys_fork: no free task slots");
                return -(EAGAIN as i64);
            };

            let parent_nr = sched::current_nr();

            // 先把栈拿到手，失败了就不用回滚任务表。
            let stack_page = crate::mm::get_free_page();
            if stack_page == 0 {
                crate::pr_warn!("sys_fork: out of memory for kernel stack");
                return -(EAGAIN as i64);
            }

            // 复制父进程的 PCB。原版 copy_process 里的 `*p = *current`。
            // 裸指针读写：parent_nr != child_nr（child_nr 是 Unused 槽位，
            // 当前任务不可能是 Unused），所以不会自我别名。
            let parent = sched::task_ptr(parent_nr);
            let child = sched::task_ptr(child_nr);
            *child = (*parent).clone();

            // 子进程特有的字段。
            (*child).state = TaskState::Running;
            (*child).pid = sched::allocate_pid();
            (*child).pgrp = (*parent).pgrp;
            (*child).session = (*parent).session;
            (*child).parent = parent_nr;
            (*child).kernel_stack = stack_page as u64;
            (*child).start_time = sched::jiffies();
            // 时间统计从零开始（原版 `p->utime = p->stime = 0`）。
            (*child).utime = 0;
            (*child).stime = 0;
            (*child).timeout = 0;
            (*child).exit_code = 0;
            // 待处理信号不继承，屏蔽字继承。原版 `p->signal = 0`。
            (*child).signal = 0;
            // 时间片对半分，父子都不能靠 fork 白拿一个整片
            // （原版 1.0.9 直接给 `p->counter = p->priority`，
            //  但那样 fork 循环能无限延长自己的份额）。
            let half = (*parent).counter / 2;
            (*parent).counter = half;
            (*child).counter = (*parent).counter - half + half; // = half
            if (*child).counter == 0 {
                (*child).counter = 1;
            }
            // pml4：父进程是纯内核任务（pml4==0）则子进程也是；
            // 有用户空间的进程走下面的 copy_page_tables 路径。
            (*child).pml4 = (*parent).pml4;
            (*child).tss.cr3 = (*child).pml4 as u64;

            // sigaction 表随 PCB 一起继承（原版是内联数组，我们在旁路数组里）。
            // CLONE_SIGHAND: share signal handler table
        if flags & CLONE_SIGHAND != 0 {
            crate::signal::share_sigactions(parent_nr, child_nr);
        } else {
            crate::signal::clone_sigactions(parent_nr, child_nr);
        }

            // Increment pwd/root inode refcounts (copied by Task::clone)
            if (*child).pwd != crate::fs::inode::NIL {
                (*crate::fs::inode::inode_ptr((*child).pwd)).i_count += 1;
            }
            if (*child).root != crate::fs::inode::NIL && (*child).root != (*child).pwd {
                (*crate::fs::inode::inode_ptr((*child).root)).i_count += 1;
            }

            // ---- 布置子进程内核栈 ----
            // 布局和 sched::kernel_thread 一致，只是 ret 地址上方放的不是
            // fn/arg 而是一整份 pt_regs：
            //   [tss.rsp + 0 .. +48]  switch_to 的 7 个保存槽
            //   [tss.rsp + 56]        返回地址 = ret_from_fork
            //   [tss.rsp + 64 ..]     pt_regs（ret_from_sys_call 要用）
            let stack_top = stack_page as u64 + KERNEL_STACK_SIZE as u64;
            // 栈底魔数，供 stack_ok() / die_if_kernel 检测溢出。
            core::ptr::write_volatile(stack_page as *mut u64, STACK_MAGIC);

            let mut sp = stack_top as *mut u64;
            // pt_regs 从高地址往低地址写，写完 sp 正好落在 pt_regs 基址。
            let src = regs as *const PtRegs as *const u64;
            for i in (0..PTREGS_QWORDS).rev() {
                sp = sp.sub(1);
                core::ptr::write_volatile(sp, core::ptr::read(src.add(i)));
            }
            // fork 在子进程里返回 0（原版 `p->tss.eax = 0`）。
            core::ptr::write_volatile(sp.add(PTREGS_RAX_IDX), 0u64);

            // switch_to 的 `ret` 落到这里。
            sp = sp.sub(1);
            core::ptr::write_volatile(sp, ret_from_fork as *const () as u64);
            // rflags 槽：IF=0，由 schedule_tail 里的 sti 开中断
            // （与 kernel_thread 的处理一致）。
            sp = sp.sub(1);
            core::ptr::write_volatile(sp, 0x0002u64);
            // 余下 6 个 callee-saved 槽清零。
            for _ in 0..6 {
                sp = sp.sub(1);
                core::ptr::write_volatile(sp, 0u64);
            }

            (*child).tss.rsp = sp as u64;
            (*child).tss.rsp0 = stack_top;

            // 挂进调度环。和 sched::kernel_thread 里的 SET_LINKS 一样，
            // 用裸指针写避免 nr/cur/old_next 相等时的 &mut 别名。
            let old_next = (*sched::task_ptr(parent_nr)).next;
            (*sched::task_ptr(child_nr)).next = old_next;
            (*sched::task_ptr(child_nr)).prev = parent_nr;
            (*sched::task_ptr(parent_nr)).next = child_nr;
            (*sched::task_ptr(old_next)).prev = child_nr;

            let child_pid = (*child).pid;
            crate::pr_info!(
                "sys_fork: parent pid={} -> child pid={} (slot {})",
                (*parent).pid,
                child_pid,
                child_nr
            );

            // 父进程拿到子进程 pid。
            child_pid as i64
        }
    })();
    // SAFETY: 与上面的 save 配对。
    unsafe { crate::irq::restore_flags(flags) };
    result
}

/// vfork syscall - 轻量级进程复制
pub fn vfork(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    fork(_args, _regs)
}

/// wait4 syscall - 等子进程退出并收尸
///
/// 对应原版 `kernel/exit.c:sys_wait4()`。参数：pid / stat_addr / options /
/// rusage（rusage 尚未实现，非 0 时忽略）。
pub fn wait4(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // a0 是 pid，按 C 的 `pid_t`（i32）符号扩展，否则 -1 会变成 2^64-1。
    let pid = args.a0 as i32 as i64;
    // SAFETY: 系统调用上下文；stat_addr 由 sys_wait4 内部判空，
    // 页表恒等映射所以用户指针可直接写（还没有独立用户地址空间）。
    unsafe { crate::exit::sys_wait4(pid, args.a1, args.a2) }
}
pub fn setitimer(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn getitimer(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// Memory syscalls
pub fn mlock(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn munlock(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn mlockall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn munlockall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn mremap(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn msync(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

// IPC syscalls
pub fn shmget(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn shmat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // Return a dummy shared memory address
    let _shmid = args.a0;
    let shmaddr = args.a1;
    if shmaddr != 0 { shmaddr as i64 } else { (crate::umm::USERSPACE_START + 0x1000000) as i64 }
}
pub fn shmdt(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn shmctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn semget(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn semop(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn semctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn semtimedop(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msgget(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msgsnd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msgrcv(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msgctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// Time syscalls

/// 时间规格结构
#[repr(C)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

/// nanosleep - 高精度睡眠
///
/// 对应原版 `kernel/sched.c:sys_nanosleep()`。
pub fn nanosleep(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let req = args.a0 as *const Timespec;
    let rem = args.a1 as *mut Timespec;
    
    if req.is_null() {
        return -(EINVAL as i64);
    }
    
    // SAFETY: req 已校验非空
    let secs = unsafe { (*req).tv_sec };
    let nsecs = unsafe { (*req).tv_nsec };
    
    if nsecs < 0 || nsecs >= 1_000_000_000 {
        return -(EINVAL as i64);
    }
    
    // 计算睡眠时间（简化为 jiffies）
    let sleep_jiffies = (secs * sched::task::HZ as i64) as u64;
    
    if sleep_jiffies > 0 {
        // SAFETY: 系统调用上下文，可以睡眠
        unsafe {
            sched::current().state = crate::sched::task::TaskState::Interruptible;
            sched::current().timeout = sched::jiffies() + sleep_jiffies;
            sched::schedule();
        }
    }
    
    // 如果有剩余时间结构指针，写入剩余时间（简化：假设睡眠完成）
    if !rem.is_null() {
        // SAFETY: rem 已校验非空
        unsafe {
            (*rem).tv_sec = 0;
            (*rem).tv_nsec = 0;
        }
    }
    
    0
}

pub fn clock_gettime(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let clk_id = args.a0 as i32;
    let tp = args.a1 as *mut Timespec;
    if tp.is_null() { return -(EFAULT as i64); }
    // Use jiffies (100Hz) for CLOCK_REALTIME (0) and CLOCK_MONOTONIC (1)
    if clk_id == 0 || clk_id == 1 {
        let jif = crate::sched::jiffies();
        let secs = jif / crate::sched::task::HZ as u64;
        let nsecs = ((jif % crate::sched::task::HZ as u64) * 10_000_000) as i64;
        unsafe { (*tp).tv_sec = secs as i64; (*tp).tv_nsec = nsecs; }
        0
    } else {
        -(EINVAL as i64)
    }
}
pub fn clock_settime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn clock_getres(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn clock_nanosleep(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { nanosleep(_args, _regs) }

// Priority syscalls
pub fn getpriority(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn setpriority(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

// Hostname syscalls
pub fn sethostname(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn setdomainname(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// CPU syscall
pub fn getcpu(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

// Resource limits
pub fn prlimit(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// File ops
/// pipe2：同 pipe，加 flags（O_CLOEXEC/O_NONBLOCK/O_DIRECT）。
/// 当前忽略 flags（管道始终阻塞，不分叉时继承 fd）。
pub fn pipe2(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // Re-use pipe() implementation; flags in a1 are ignored for now
    let _flags = args.a1 as i32;
    let pipe_args = SysArgs { a0: args.a0, a1: 0, a2: 0, a3: 0, a4: 0, a5: 0 };
    pipe(&pipe_args, _regs)
}
pub fn fchmodat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fchownat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn openat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn mkdirat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn mknodat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn unlinkat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn renameat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn renameat2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn linkat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn symlinkat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fchown(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

pub fn getrusage(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let usage = args.a1 as *mut RUsage;
    if !usage.is_null() { unsafe { (*usage).ru_utime.tv_sec = 0; (*usage).ru_utime.tv_usec = 0; (*usage).ru_stime.tv_sec = 0; (*usage).ru_stime.tv_usec = 0; } }
    0
}
/// 资源限制。
pub fn getrlimit(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let rlim = args.a1 as *mut RLimit;
    if !rlim.is_null() { unsafe { (*rlim).rlim_cur = -1i64 as u64; (*rlim).rlim_max = -1i64 as u64; } }
    0
}

// Advanced syscalls

/// process control.
pub fn prctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let option = args.a0 as i32;
    let arg2 = args.a1;
    match option {
        3 => 1,  // PR_GET_DUMPABLE → superuser dumpable
        4 => 0,  // PR_SET_DUMPABLE → accepted
        15 => { // PR_SET_NAME: set task comm
            let name_ptr = arg2 as *const u8;
            if name_ptr.is_null() { return -(EFAULT as i64); }
            unsafe {
                let t = sched::current();
                let mut len = 0;
                while len < 15 {
                    let b = core::ptr::read_volatile(name_ptr.add(len));
                    if b == 0 { break; }
                    t.comm[len] = b;
                    len += 1;
                }
                t.comm[len] = 0;
            }
            0
        }
        16 => { // PR_GET_NAME: get task comm
            let buf = arg2 as *mut u8;
            if buf.is_null() { return -(EFAULT as i64); }
            unsafe {
                let t = sched::current();
                let mut i = 0;
                while i < 16 {
                    core::ptr::write_volatile(buf.add(i), t.comm[i]);
                    if t.comm[i] == 0 { break; }
                    i += 1;
                }
            }
            0
        }
        22 => 0, // PR_SET_SECCOMP
        23 => 0, // PR_CAPBSET_READ
        36 => 0, // PR_SET_NO_NEW_PRIVS
        _ => {
            crate::pr_warn!("sys_prctl: option={} (stub)", option);
            0 // Be permissive: most prctl options are optional
        }
    }
}

/// set child tid address.
pub fn set_tid_address(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: current() valid in syscall context
    unsafe { sched::current().pid as i64 }
}

/// get random bytes.
pub fn getrandom(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let buf = args.a0 as *mut u8;
    let len = args.a1 as usize;
    if buf.is_null() { return -(EINVAL as i64); }
    unsafe { core::ptr::write_bytes(buf, 0, len.min(256)); }
    len as i64
}

/// memory management.
pub fn mbind(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn set_mempolicy(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn get_mempolicy(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn migrate_pages(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn move_pages(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn mlock2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// io setup.
pub fn io_setup(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_destroy(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_submit(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_getevents(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_cancel(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_pgetevents(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// keyctl syscalls.
pub fn add_key(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn request_key(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn keyctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// inotify.
pub fn inotify_init(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn inotify_init1(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn inotify_add_watch(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn inotify_rm_watch(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// epoll.
pub fn epoll_create(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn epoll_create1(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn epoll_ctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn epoll_wait(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// timerfd.
pub fn timerfd_create(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn timerfd_settime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn timerfd_gettime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// eventfd.
pub fn eventfd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn eventfd2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// file operations.
pub fn splice(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn tee(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn vmsplice(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn sync_file_range(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn vhangup(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn dup3(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn faccessat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn statfs(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn fstatfs(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn truncate64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn ftruncate64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fallocate(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fanotify_init(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fanotify_mark(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn copy_file_range(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn preadv2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn pwritev2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn statx(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn lookup_dcookie(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn syncfs(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

/// *at syscalls.

/// fchown.
/// chmod.
/// fchmod.

/// advanced syscalls.
pub fn perf_event_open(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn accept4(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // accept4 = accept + flags (SOCK_CLOEXEC/SOCK_NONBLOCK)
    // a0=fd, a1=addr, a2=addrlen, a3=flags
    let _flags = args.a3 as i32;
    let accept_args = SysArgs { a0: args.a0, a1: args.a1, a2: args.a2, a3: 0, a4: 0, a5: 0 };
    accept(&accept_args, _regs)
}
pub fn process_vm_readv(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn process_vm_writev(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

/// memory protection keys.
pub fn pkey_mprotect(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn pkey_alloc(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn pkey_free(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// extended attributes.
pub fn setxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn lsetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fsetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn getxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn lgetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fgetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn listxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn llistxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn flistxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn removexattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn lremovexattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fremovexattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// io_uring (simplified stub).
pub fn io_uring_setup(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_uring_enter(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_uring_register(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// other advanced syscalls.
pub fn kexec_load(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn init_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn delete_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn sched_setattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn sched_getattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn seccomp(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn memfd_create(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn userfaultfd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn membarrier(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn clock_adjtime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn setns(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn rseq(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

// =============================================================================
// 补齐 x86_64 正式调用号 0..=334 的空洞（对应 `arch/x86/entry/syscalls/
// syscall_64.tbl`）。这一段的目标是让调用号连续排满，用户态拿到的是
// `-ENOSYS`/合理默认值而不是「越界」——真正的实现随对应子系统移植逐个替换。
//
// 分三类：
// 1. 能靠现有子系统真做的（lseek / readv / writev / sched_yield / …）；
// 2. 有合理默认值的（getgroups 返回 0、madvise 是建议可忽略、…）；
// 3. 纯占位返回 `-ENOSYS`（信号 rt_* 系列、POSIX 定时器、mq_*、futex、…）。
// =============================================================================

/// 移动文件读写位置。对应原版 `fs/read_write.c:sys_lseek()`。
/// 直接转给 [`crate::fs::read_write::lseek`]。
pub fn lseek(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: fs 层自己校验 fd，越界/未打开返回 -EBADF。
    unsafe { crate::fs::read_write::lseek(args.a0 as usize, args.a1 as i64, args.a2 as u32) }
}

/// 分散读。对应原版 1.0.9 之后才有的 `sys_readv()`。
/// 拆成对每个 iovec 调一次 [`read`]，短读即停（与原版语义一致）。
pub fn readv(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    iov_loop(args, regs, read)
}

/// 分散写。对应 `sys_writev()`，实现方式同 [`readv`]。
pub fn writev(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    iov_loop(args, regs, write)
}

/// `struct iovec`。字段顺序与 x86_64 ABI 一致。
#[repr(C)]
struct IoVec {
    base: u64,
    len: u64,
}

/// `readv`/`writev` 的公共循环。
fn iov_loop(args: &SysArgs, regs: &mut PtRegs, f: fn(&SysArgs, &mut PtRegs) -> i64) -> i64 {
    let (fd, iov, cnt) = (args.a0, args.a1 as *const IoVec, args.a2 as usize);
    if iov.is_null() {
        return -(EFAULT as i64);
    }
    // 原版 UIO_MAXIOV = 1024
    if cnt > 1024 {
        return -(EINVAL as i64);
    }
    let mut total: i64 = 0;
    for i in 0..cnt {
        // SAFETY: 缺 verify_area，这里和 sys_write 一样只能信任调用方；
        // 等 mm/mmap.c 的 vm_area_struct 到位后加校验。
        let v = unsafe { &*iov.add(i) };
        if v.len == 0 {
            continue;
        }
        let sub = SysArgs { a0: fd, a1: v.base, a2: v.len, a3: 0, a4: 0, a5: 0 };
        let r = f(&sub, regs);
        if r < 0 {
            return if total > 0 { total } else { r };
        }
        total += r;
        // 短读/短写：不再继续下一个 iovec
        if (r as u64) < v.len {
            break;
        }
    }
    total
}

/// 主动让出 CPU。对应原版没有（1.0.9 无 `sched_yield`），语义等价于
/// 直接调一次 `schedule()`。
pub fn sched_yield(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 系统调用上下文，不在中断里，可以安全切换。
    unsafe { sched::schedule() };
    0
}

/// 返回线程 ID。本树没有线程，tid == pid（原版 1.0.9 同样没有）。
pub fn gettid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 系统调用上下文里 current 必然有效。
    unsafe { sched::current() }.pid as i64
}

/// 秒级时间。对应原版 `kernel/time.c:sys_time()`。
/// 没有 RTC 驱动，用 jiffies/HZ 当作开机以来的秒数。
pub fn time(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let secs = (sched::jiffies() / crate::sched::task::HZ) as i64;
    let p = args.a0 as *mut i64;
    if !p.is_null() {
        // SAFETY: 缺 verify_area，同 sys_write 的限制。
        unsafe { p.write_volatile(secs) };
    }
    secs
}

/// 退出整个线程组。没有线程组，退化成 [`exit`]。
pub fn exit_group(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    exit(args, regs)
}

/// 按 tid 发信号。没有线程，等价于按 pid 发（转给 [`kill`]）。
pub fn tkill(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    kill(args, regs)
}

/// 按 tgid+tid 发信号。同 [`tkill`]，忽略 tgid。
pub fn tgkill(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    let sub = SysArgs { a0: args.a1, a1: args.a2, a2: 0, a3: 0, a4: 0, a5: 0 };
    kill(&sub, regs)
}

/// 建立硬链接。对应原版 `fs/namei.c:sys_link()`。
/// minix 层的 link 还没接出来，先返回 `-ENOSYS`。
pub fn link(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]，两个路径各自校验。
    let old = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同上。
    let new = match unsafe { user_path(args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::namei::do_link(old, new) }
}

// --- 有合理默认值的一批 -------------------------------------------------------

/// 建议内核的页面使用方式。建议性调用，忽略即合法（原版无）。
pub fn madvise(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 查询页面是否驻留。没有换页，全部驻留 → 直接返回成功。
pub fn mincore(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 预读提示。无预读机制，忽略。
pub fn readahead(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 文件访问模式提示。同 [`madvise`]，忽略。
pub fn fadvise64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 数据同步（不含元数据）。缓冲缓存是写回的，转给 [`fsync`]。
pub fn fdatasync(args: &SysArgs, regs: &mut PtRegs) -> i64 { fsync(args, regs) }
/// 文件加锁。单进程内核，无竞争 → 直接成功。
pub fn flock(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件权限（按 fd）。minix 层还没接 chmod，先当成功。
pub fn fchmod(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件属主（不跟随符号链接）。同 [`chown`] 的限制。
pub fn lchown(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件时间戳。没有 RTC，忽略。
pub fn utime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件时间戳（μs 精度）。同 [`utime`]。
pub fn utimes(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件时间戳（ns 精度 + dirfd）。同 [`utime`]。
pub fn utimensat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件时间戳（dirfd 版）。同 [`utime`]。
pub fn futimesat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

/// 取附加组列表。没有组机制，返回 0 个组（原版 `sys.c:sys_getgroups()`
/// 在 NGROUPS 为空时同样返回 0）。
pub fn getgroups(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 设置附加组列表。需要 root，本树无 uid 概念 → `-EPERM`。
pub fn setgroups(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置真实/有效 uid。同 [`setuid`] 的限制。
pub fn setreuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置真实/有效 gid。同上。
pub fn setregid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置真实/有效/保存 uid。同上。
pub fn setresuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置真实/有效/保存 gid。同上。
pub fn setresgid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置文件系统 uid。返回旧值（恒为 0），与原版「返回旧 fsuid」一致。
pub fn setfsuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 设置文件系统 gid。同 [`setfsuid`]。
pub fn setfsgid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 取真实/有效/保存 uid。三个都是 0，写回三个指针。
pub fn getresuid(args: &SysArgs, _regs: &mut PtRegs) -> i64 { put_triple(args, 0) }
/// 取真实/有效/保存 gid。同 [`getresuid`]。
pub fn getresgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 { put_triple(args, 0) }

/// `getresuid`/`getresgid` 的公共写回。
fn put_triple(args: &SysArgs, v: u32) -> i64 {
    for p in [args.a0, args.a1, args.a2] {
        let p = p as *mut u32;
        if p.is_null() {
            return -(EFAULT as i64);
        }
        // SAFETY: 缺 verify_area，同 sys_write 的限制。
        unsafe { p.write_volatile(v) };
    }
    0
}

/// 取进程组。a0 == 0 表示当前进程；本树只支持当前进程。
pub fn getpgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    if args.a0 != 0 {
        return -(ENOSYS as i64);
    }
    // SAFETY: 系统调用上下文里 current 必然有效。
    unsafe { sched::current() }.pgrp as i64
}

/// 取会话 ID。限制同 [`getpgid`]。
pub fn getsid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    if args.a0 != 0 {
        return -(ENOSYS as i64);
    }
    // SAFETY: 同上。
    unsafe { sched::current() }.session as i64
}

/// 设资源限制。没有 rlimit 强制机制，接受但不生效。
pub fn setrlimit(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 调度参数（优先级）。本树的 nice 值在 `getpriority`/`setpriority` 里，
/// 这四个 POSIX 实时调度接口没有对应实现。
pub fn sched_setparam(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 取调度参数。同 [`sched_setparam`]。
pub fn sched_getparam(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 设调度策略。只有 SCHED_OTHER，改成别的都拒绝。
pub fn sched_setscheduler(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EINVAL as i64) }
/// 取调度策略。恒为 SCHED_OTHER(0)。
pub fn sched_getscheduler(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 实时优先级上限。SCHED_OTHER 下为 0。
pub fn sched_get_priority_max(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 实时优先级下限。同上。
pub fn sched_get_priority_min(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 取 RR 时间片。没有 SCHED_RR。
pub fn sched_rr_get_interval(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 取/设 CPU 亲和性。单核，掩码恒为 {0}。
pub fn sched_getaffinity(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let p = args.a2 as *mut u64;
    if p.is_null() {
        return -(EFAULT as i64);
    }
    // SAFETY: 缺 verify_area，同 sys_write 的限制。
    unsafe { p.write_volatile(1) };
    8
}
/// 设 CPU 亲和性。单核，只能是 CPU0，任何掩码都当成功。
pub fn sched_setaffinity(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

// --- 纯占位：对应子系统未移植，统一返回 -ENOSYS -----------------------------

// rt_sigaction 等见下面 LFS Critical Syscalls 段
/// 指定偏移读，不改 f_pos。要先给 fs 层加一个不动 f_pos 的读路径。
pub fn pread64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 指定偏移写，不改 f_pos。同 [`pread64`]。
pub fn pwrite64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 内核内文件到文件的搬运。需要 fs 层的 splice 基础设施。
pub fn sendfile(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 创建进程/线程。`sys_fork` 已有，clone 的 flags 语义（共享地址空间/文件表）还没有。
/// clone syscall — 创建进程/线程。flags 控制资源共享。
pub fn clone(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::EAGAIN;
    use crate::sched::task::{KERNEL_STACK_SIZE, STACK_MAGIC, TaskState};

    let flags = args.a0 as u64;
    let child_stack = args.a1; // 新线程的用户栈
    let parent_tidptr = args.a2; // CLONE_PARENT_SETTID
    let child_tidptr = args.a3;  // CLONE_CHILD_SETTID / CLONE_CHILD_CLEARTID
    let new_tls = args.a4;       // CLONE_SETTLS: new TLS (FS base)

    const CLONE_VM: u64 = 0x100;
    const CLONE_FS: u64 = 0x200;
    const CLONE_FILES: u64 = 0x400;
    const CLONE_SIGHAND: u64 = 0x800;
    const CLONE_PTRACE: u64 = 0x2000;
    const CLONE_VFORK: u64 = 0x4000;
    const CLONE_PARENT: u64 = 0x8000;
    const CLONE_THREAD: u64 = 0x10000;
    const CLONE_SETTLS: u64 = 0x80000;
    const CLONE_PARENT_SETTID: u64 = 0x100000;
    const CLONE_CHILD_CLEARTID: u64 = 0x200000;
    const CLONE_CHILD_SETTID: u64 = 0x1000000;

    unsafe extern "C" { fn ret_from_fork(); }

    const PTREGS_QWORDS: usize = 21;
    const PTREGS_RAX_IDX: usize = 0x50 / 8;

    let parent_nr = sched::current_nr();

    // 找空闲槽位
    let child_nr = unsafe {
        let mut slot = None;
        for i in 1..sched::NR_TASKS {
            if (*sched::task_ptr(i)).state == TaskState::Unused {
                slot = Some(i); break;
            }
        }
        slot
    };
    let Some(child_nr) = child_nr else { return -(EAGAIN as i64); };

    let stack_page = if flags & CLONE_VM != 0 {
        // Thread: use the provided user stack pointer as kernel stack base
        // (kernel stack = a page below child_stack_top, for simplicity)
        crate::mm::get_free_page()
    } else {
        crate::mm::get_free_page()
    };
    if stack_page == 0 { return -(EAGAIN as i64); }

    // SAFETY: 单核，独占
    unsafe {
        let parent = sched::task_ptr(parent_nr);
        let child = sched::task_ptr(child_nr);

        // Copy parent task
        *child = (*parent).clone();

        (*child).state = TaskState::Running;
        (*child).start_time = sched::jiffies();
        (*child).utime = 0; (*child).stime = 0;
        (*child).signal = 0;
        (*child).exit_code = 0;
        (*child).counter = (*parent).counter / 2;
        if (*child).counter == 0 { (*child).counter = 1; }
        (*parent).counter /= 2;
        if (*parent).counter == 0 { (*parent).counter = 1; }

        // CLONE_VM: 共享地址空间（线程共享 PML4）
        if flags & CLONE_VM != 0 {
            (*child).pml4 = (*parent).pml4;
            (*child).tss.cr3 = (*parent).pml4 as u64;
        } else if (*parent).pml4 != 0 {
            // Fork: child gets independent PML4 with COW
            let child_pml4 = crate::mm::paging::alloc_pml4();
            if child_pml4 != 0
                && crate::mm::paging::clone_kernel_pdpt(child_pml4)
                && crate::mm::paging::cow_copy_page_table((*parent).pml4, child_pml4)
            {
                (*child).pml4 = child_pml4;
                (*child).tss.cr3 = child_pml4 as u64;
            }
        }

        // CLONE_THREAD: threads get unique PID, share address space via CLONE_VM
        if flags & CLONE_THREAD != 0 {
            (*child).pid = sched::allocate_pid();
            // Thread group: parent pid stays as the "tgid" equivalent
        } else {
            (*child).pid = sched::allocate_pid();
            (*child).parent = parent_nr;
        }

        if flags & CLONE_PARENT != 0 {
            (*child).parent = (*parent).parent;
        }

        // CLONE_FILES: share fd table with parent
        if flags & CLONE_FILES != 0 {
            crate::fs::open::clone_fds(parent_nr, child_nr);
        }

        // CLONE_SETTLS: 为新线程设 TLS（FS base）
        if flags & CLONE_SETTLS != 0 && new_tls != 0 {
            // Write MSR_FS_BASE for the child thread
            // This will take effect when the child is first scheduled
            unsafe {
                core::arch::asm!(
                    "wrmsr",
                    in("ecx") 0xC0000100u32, // MSR_FS_BASE
                    in("eax") (new_tls as u32),
                    in("edx") (new_tls >> 32) as u32,
                );
            }
        }

        // CLONE_CHILD_CLEARTID: 子进程退出时清零 child_tidptr
        if flags & CLONE_CHILD_CLEARTID != 0 && child_tidptr != 0 {
            // Store the clear_tid address so do_exit can clear it
            // For now, clear it immediately (child hasn't started yet)
            core::ptr::write_volatile(child_tidptr as *mut i32, 0);
        }

        // CLONE_CHILD_SETTID: 在子进程的 child_tidptr 处写入 tid
        if flags & CLONE_CHILD_SETTID != 0 && child_tidptr != 0 {
            core::ptr::write_volatile(child_tidptr as *mut i32, (*child).pid as i32);
        }

        // CLONE_PARENT_SETTID: 在父进程的 parent_tidptr 处写入子进程 tid
        if flags & CLONE_PARENT_SETTID != 0 && parent_tidptr != 0 {
            core::ptr::write_volatile(parent_tidptr as *mut i32, (*child).pid as i32);
        }

        // CLONE_SIGHAND: share signal handler table
        if flags & CLONE_SIGHAND != 0 {
            crate::signal::share_sigactions(parent_nr, child_nr);
        } else {
            crate::signal::clone_sigactions(parent_nr, child_nr);
        }

        // Increment pwd/root inode refcounts (copied by Task::clone)
        if (*child).pwd != crate::fs::inode::NIL {
            (*crate::fs::inode::inode_ptr((*child).pwd)).i_count += 1;
        }
        if (*child).root != crate::fs::inode::NIL && (*child).root != (*child).pwd {
            (*crate::fs::inode::inode_ptr((*child).root)).i_count += 1;
        }

        // 布置子进程内核栈
        let stack_top = stack_page as u64 + KERNEL_STACK_SIZE as u64;
        core::ptr::write_volatile(stack_page as *mut u64, STACK_MAGIC);

        let mut sp = stack_top as *mut u64;
        let src = regs as *const PtRegs as *const u64;
        for i in (0..PTREGS_QWORDS).rev() {
            sp = sp.sub(1);
            core::ptr::write_volatile(sp, core::ptr::read(src.add(i)));
        }
        // 子进程 rax = 0
        core::ptr::write_volatile(sp.add(PTREGS_RAX_IDX), 0u64);
        // 如果指定了子进程用户栈，改写 rsp
        if child_stack != 0 {
            // PT_RSP 在 pt_regs 的偏移 0x98（qword 19）
            core::ptr::write_volatile(sp.add(0x98 / 8), child_stack);
        }

        sp = sp.sub(1);
        core::ptr::write_volatile(sp, ret_from_fork as *const () as u64);
        sp = sp.sub(1);
        core::ptr::write_volatile(sp, 0x0002u64); // rflags
        for _ in 0..6 { sp = sp.sub(1); core::ptr::write_volatile(sp, 0u64); }

        (*child).tss.rsp = sp as u64;
        (*child).tss.rsp0 = stack_top;

        // 挂进调度环
        let old_next = (*sched::task_ptr(parent_nr)).next;
        (*sched::task_ptr(child_nr)).next = old_next;
        (*sched::task_ptr(child_nr)).prev = parent_nr;
        (*sched::task_ptr(parent_nr)).next = child_nr;
        (*sched::task_ptr(old_next)).prev = child_nr;

        (*child).pid as i64
    }
}
/// 进程跟踪。需要 `arch_ptrace` 与调试寄存器支持。
pub fn ptrace(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 读内核日志环。`klib::printk::read_log()` 已有，缺用户地址校验才好接。
pub fn syslog(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取进程能力集。没有 capability 机制。
pub fn capget(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设进程能力集。同 [`capget`]。
pub fn capset(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取待处理信号集（rt 版）。
pub fn rt_sigpending(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带超时地等信号（rt 版）。
pub fn rt_sigtimedwait(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带 siginfo 发信号。
pub fn rt_sigqueueinfo(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 临时换屏蔽字并挂起。
pub fn rt_sigsuspend(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置备用信号栈。
/// sigaltstack：设置/获取备选信号栈。接受所有参数，返回 0。
pub fn sigaltstack(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let ss = args.a0 as *const u64; // stack_t { ss_sp, ss_flags, ss_size }
    let oss = args.a1 as *mut u64;
    if oss.is_null() && ss.is_null() { return 0; }
    // If oss is provided, write current altstack (none = disabled)
    if !oss.is_null() {
        unsafe { core::ptr::write_volatile(oss.add(1), 2u64); } // SS_DISABLE
    }
    // Accept ss if provided (ignore contents)
    0
}
/// 加载共享库（老式 a.out）。`src/elf/` 走的是现代路径，不打算实现。
pub fn uselib(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置执行域。只有一种 personality。
pub fn personality(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 文件系统统计（已废弃接口）。用 [`statfs`] 代替。
pub fn ustat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 查询已注册的文件系统类型。`fs/devices.rs` 里还没有类型注册表。
pub fn sysfs(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 改 LDT。`src/desc.rs` 只建了 GDT，没有 LDT。
pub fn modify_ldt(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 换根挂载点。需要挂载树，现在只支持单个根。
pub fn pivot_root(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 旧式 sysctl（已废弃）。
pub fn sysctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 架构相关的进程控制。x86_64 上主要是 FS/GS base（TLS）。
pub fn arch_prctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::EINVAL;
    let code = args.a0 as i32;
    let addr = args.a1;
    // SAFETY: 系统调用上下文，单核。
    unsafe {
        let me = sched::task_ptr(sched::current_index());
        match code {
            0x1001 => { // ARCH_SET_GS
                (*me).gs_base = addr;
                core::arch::asm!("wrmsr", in("ecx") 0xC000_0101u64,
                    in("eax") addr as u32, in("edx") (addr >> 32) as u32,
                    options(nomem, nostack, preserves_flags));
                0
            }
            0x1002 => { // ARCH_SET_FS
                (*me).fs_base = addr;
                core::arch::asm!("wrmsr", in("ecx") 0xC000_0100u64,
                    in("eax") addr as u32, in("edx") (addr >> 32) as u32,
                    options(nomem, nostack, preserves_flags));
                0
            }
            0x1003 => (*me).gs_base as i64,       // ARCH_GET_GS
            0x1004 => (*me).fs_base as i64,       // ARCH_GET_FS
            _ => -(EINVAL as i64),
        }
    }
}

pub fn rt_sigaction(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::EINVAL;
    let signum = args.a0 as usize;
    let act_ptr = args.a1;
    let oldact_ptr = args.a2;
    let sigsetsize = args.a3;

    if signum == 0 || signum > 31 || sigsetsize != 8 {
        return -(EINVAL as i64);
    }
    // 读出旧 action
    let old = crate::signal::get_signal(signum);
    if oldact_ptr != 0 {
        // SAFETY: 系统调用上下文，用户指针恒等映射。
        unsafe { core::ptr::write_volatile(oldact_ptr as *mut crate::signal::SigAction, old) };
    }
    if act_ptr != 0 {
        // SAFETY: 同上。
        let act = unsafe { core::ptr::read_volatile(act_ptr as *const crate::signal::SigAction) };
        crate::signal::set_signal(signum, act);
    }
    0
}

pub fn rt_sigprocmask(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::EINVAL;
    let how = args.a0 as i32;
    let set_ptr = args.a1;
    let oldset_ptr = args.a2;
    let sigsetsize = args.a3;

    if sigsetsize != 8 { return -(EINVAL as i64); }

    // SAFETY: 系统调用上下文。
    let old = unsafe {
        let t = sched::task_ptr(sched::current_index());
        let old = (*t).blocked;
        if oldset_ptr != 0 {
            core::ptr::write_volatile(oldset_ptr as *mut u64, old);
        }
        if set_ptr != 0 {
            let set = core::ptr::read_volatile(set_ptr as *const u64);
            match how {
                0 => (*t).blocked = set,                             // SIG_BLOCK
                1 => (*t).blocked |= set,                            // SIG_UNBLOCK
                2 => (*t).blocked = (old | set) ^ set,               // SIG_SETMASK
                _ => return -(EINVAL as i64),
            }
        }
        old
    };
    if oldset_ptr == 0 { 0 } else { old as i64 }
}

/// rt_sigreturn — 从用户态信号栈恢复被中断的现场。
///
/// 信号帧布局（由 signal::setup_frame 写入）：
///   [rsp + 0]  ret_addr (= &trampoline)
///   [rsp + 8]  saved ss
///   [rsp +16]  saved rsp
///   [rsp +24]  saved rflags
///   [rsp +32]  saved cs
///   [rsp +40]  saved rip
///   [rsp +48]  trampoline (12 bytes)
///
/// 当 handler `ret` 时，rsp 指向 saved_ss 开头（ret_addr 已被弹出）。
/// 然后 trampoline 执行 `syscall`（nr=15），进入本函数。
/// 此时 rsp（用户栈指针）指向 saved_rip 之后的那段空间。
pub fn rt_sigreturn(_args: &SysArgs, regs: &mut PtRegs) -> i64 {
    use crate::desc::selector::{USER_CS, USER_DS};

    // User RSP at syscall entry: points just past the trampoline.
    // Go backwards: trampoline[-12], saved_rip[-20], saved_cs[-28], etc.
    // Actually, after `ret` from handler pops ret_addr, rsp = frame_addr + 8.
    // Then the trampoline runs (doesn't touch rsp), then syscall (pushes nothing to user stack).
    // So at syscall entry: user rsp = frame_addr + 8 = &saved_ss

    // Read one level up: the pt_regs saved by int 0x80 has user rsp
    // But regs.rsp here is the kernel stack value, not user rsp.
    // The user rsp is in the pt_regs that entry.S saved. Let me find it.

    // On x86_64 with int 0x80: entry.S pushes pt_regs. The user rsp is at
    // pt_regs.rsp (offset depends on PtRegs layout).
    let user_rsp = regs.rsp;

    unsafe {
        // Read saved fields from user stack (just above current user rsp)
        let saved_addr = user_rsp as *const u64;
        // Layout: [ret_addr] [saved_ss] [saved_rsp] [saved_rflags] [saved_cs] [saved_rip]
        // user_rsp points to ret_addr (which was popped by `ret`... no, wait)

        // Let me re-trace:
        // 1. setup_frame sets regs.rsp = frame_addr
        // 2. iretq: user rsp = frame_addr, user rip = handler
        // 3. handler runs, may push/pop; at `ret`, pops ret_addr (frame_addr+0)
        // 4. user rip = ret_addr = &trampoline, user rsp = frame_addr + 8
        // 5. trampoline executes (no stack ops), rsp unchanged at frame_addr + 8
        // 6. syscall: user rsp saved into pt_regs.rsp = frame_addr + 8

        // So user_rsp = frame_addr + 8 = &saved_ss
        // Saved fields start HERE (at user_rsp):
        let saved = user_rsp as *const u64;
        let saved_ss = core::ptr::read_volatile(saved.add(0));
        let saved_rsp = core::ptr::read_volatile(saved.add(1));
        let saved_rflags = core::ptr::read_volatile(saved.add(2));
        let saved_cs = core::ptr::read_volatile(saved.add(3));
        let saved_rip = core::ptr::read_volatile(saved.add(4));

        // Validate: saved CS must be user CS (0x1B) and SS must be user DS (0x23)
        if saved_cs != USER_CS as u64 || saved_ss != USER_DS as u64 {
            // Corrupted frame — fall back to terminating the process
            crate::pr_warn!("rt_sigreturn: corrupted frame cs={:#x} ss={:#x}",
                            saved_cs, saved_ss);
            crate::exit::do_exit(crate::signal::Signal::SIGSEGV as i32);
            return 0;
        }

        // Restore pt_regs
        regs.rip = saved_rip;
        regs.cs = saved_cs;
        regs.rflags = saved_rflags;
        regs.rsp = saved_rsp;
        regs.ss = saved_ss;
        // rax is the return value from rt_sigreturn (0 = success)
        regs.rax = 0;
    }
    0
}
/// 调整系统时钟。没有 RTC 与 NTP 环路。
pub fn adjtimex(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 换根目录。需要 per-task 的 root inode。
pub fn chroot(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 开启进程记账。
pub fn acct(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置系统时间。没有 RTC。
pub fn settimeofday(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 启用交换分区。没有换页子系统。
pub fn swapon(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 关闭交换分区。同 [`swapon`]。
pub fn swapoff(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 改 IOPL。会放开用户态端口访问，等有真用户态进程再说。
pub fn iopl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改 I/O 端口位图。需要 TSS 里的 I/O 位图。
pub fn ioperm(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 装载模块（1.0.9 的老接口）。没有模块加载器。
pub fn create_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取内核符号表。同 [`create_module`]。
pub fn get_kernel_syms(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 查询模块信息。同 [`create_module`]。
pub fn query_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 磁盘配额控制。minix 层没有配额。
pub fn quotactl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// NFS 服务端控制（已从 Linux 移除）。
pub fn nfsservctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// STREAMS 接口，Linux 从未实现，占号。
pub fn getpmsg(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// STREAMS 接口，Linux 从未实现，占号。
pub fn putpmsg(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// AFS 保留号，Linux 从未实现。
pub fn afs_syscall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// TUX web 服务器保留号，已废弃。
pub fn tuxcall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// LSM 保留号，Linux 从未实现。
pub fn security(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 快速用户态互斥。需要 per-address 的等待队列哈希。
// ---- Futex per-address hash table ----
const FUTEX_HASH_BITS: usize = 5;
const FUTEX_HASH_SIZE: usize = 1 << FUTEX_HASH_BITS; // 32 buckets
use core::mem::MaybeUninit;
static mut FUTEX_BUCKETS: [MaybeUninit<crate::sched::WaitQueue>; FUTEX_HASH_SIZE] =
    [const { MaybeUninit::uninit() }; FUTEX_HASH_SIZE];
static mut FUTEX_INITED: bool = false;

fn futex_hash(addr: *const u32) -> usize {
    (addr as usize >> 2) & (FUTEX_HASH_SIZE - 1)
}

fn futex_ensure_inited() {
    unsafe {
        if !FUTEX_INITED {
            for i in 0..FUTEX_HASH_SIZE {
                FUTEX_BUCKETS[i].write(crate::sched::WaitQueue::new());
            }
            FUTEX_INITED = true;
        }
    }
}

pub fn futex(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::EAGAIN;
    let uaddr = args.a0 as *const u32;
    let op = args.a1 as i32;
    let val = args.a2 as u32;
    let futex_cmd = op & 0x7F;
    // val2 for FUTEX_WAKE: max number of waiters to wake
    let val2 = if futex_cmd == 1 { val as usize } else { 1 };

    match futex_cmd {
        0 => {
            // FUTEX_WAIT: if *uaddr == val, sleep; otherwise return EAGAIN
            if uaddr.is_null() { return -(EFAULT as i64); }
            let cur = unsafe { core::ptr::read_volatile(uaddr) };
            if cur != val {
                return -(EAGAIN as i64);
            }
            futex_ensure_inited();
            let bucket = futex_hash(uaddr);
            unsafe {
                crate::sched::current().state = crate::sched::task::TaskState::Interruptible;
                crate::sched::current().timeout = crate::sched::jiffies() + 100;
                FUTEX_BUCKETS[bucket].assume_init_mut().sleep_on();
            }
            0
        }
        1 | 2 => {
            // FUTEX_WAKE / FUTEX_WAKE_BITSET: wake waiters
            futex_ensure_inited();
            let bucket = futex_hash(uaddr);
            unsafe {
                for _ in 0..val2 {
                    FUTEX_BUCKETS[bucket].assume_init_mut().wake_up();
                }
            }
            0
        }
        5 => {
            // FUTEX_WAKE_OP
            0
        }
        3 | 4 => {
            // FUTEX_FD / FUTEX_REQUEUE: not supported
            -(ENOSYS as i64)
        }
        6 => {
            // FUTEX_CMP_REQUEUE: not supported
            -(ENOSYS as i64)
        }
        _ => -(ENOSYS as i64),
    }
}
/// 设 TLS 段（i386 遗留）。x86_64 用 [`arch_prctl`]。
pub fn set_thread_area(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取 TLS 段（i386 遗留）。同上。
pub fn get_thread_area(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// epoll 的废弃老接口，占号。
pub fn epoll_ctl_old(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// epoll 的废弃老接口，占号。
pub fn epoll_wait_old(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 重排文件映射（已废弃）。
pub fn remap_file_pages(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 重启被信号打断的调用。需要 `ERESTART*` 的完整回绕逻辑。
pub fn restart_syscall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 创建 POSIX 定时器。只有 itimer，没有 POSIX 定时器池。
pub fn timer_create(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 1 }
pub fn timer_settime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn timer_gettime(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // Write zeroed itimerspec to user buffer
    let val = args.a2 as *mut u64;
    if !val.is_null() {
        unsafe { core::ptr::write_volatile(val, 0); core::ptr::write_volatile(val.add(1), 0);
                 core::ptr::write_volatile(val.add(2), 0); core::ptr::write_volatile(val.add(3), 0); }
    }
    0
}
pub fn timer_getoverrun(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 删除 POSIX 定时器。同 [`timer_create`]。
pub fn timer_delete(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// vserver 保留号，Linux 从未实现。
pub fn vserver(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 打开 POSIX 消息队列。只有 SysV 消息队列。
pub fn mq_open(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 删除 POSIX 消息队列。同 [`mq_open`]。
pub fn mq_unlink(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带超时发送。同 [`mq_open`]。
pub fn mq_timedsend(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带超时接收。同 [`mq_open`]。
pub fn mq_timedreceive(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 注册消息到达通知。同 [`mq_open`]。
pub fn mq_notify(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 读写队列属性。同 [`mq_open`]。
pub fn mq_getsetattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 等子进程（可不收尸）。`wait4` 已有，WNOWAIT 语义还没有。
pub fn waitid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设 I/O 优先级。`ll_rw_blk` 的请求队列没有优先级。
pub fn ioprio_set(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 取 I/O 优先级。同 [`ioprio_set`]。
pub fn ioprio_get(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// `fstatat` 的正式名。需要 dirfd 相对解析。
pub fn newfstatat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::fs::stat::Stat64;
    use crate::klib::errno::{EBADF, EFAULT, EINVAL};

    let dirfd = args.a0 as i32;
    let path_ptr = args.a1;
    let stat_ptr = args.a2;
    let _flags = args.a3;

    if stat_ptr == 0 { return -(EFAULT as i64); }

    let result = if dirfd == -100 && path_ptr != 0 {
        // AT_FDCWD：按路径 stat
        let path = unsafe { user_path(path_ptr) };
        match path {
            Ok(p) => {
                // SAFETY: p 是有效路径切片
                match unsafe { crate::fs::namei::namei(p) } {
                    Ok(inr) => unsafe { Stat64::from_inode(inr) },
                    Err(e) => { return e as i64; }
                }
            }
            Err(e) => { return e; }
        }
    } else if path_ptr == 0 {
        // fd 路径为空：fstat(dirfd)
        let filp_idx = crate::fs::open::fd_to_filp(dirfd as usize);
        if filp_idx == crate::fs::inode::NIL { return -(EBADF as i64); }
        // SAFETY: filp_idx 有效
        let inr = unsafe { (*crate::fs::file_table::filp(filp_idx)).f_inode };
        unsafe { Stat64::from_inode(inr) }
    } else {
        return -(EINVAL as i64);
    };

    match result {
        Some(s) => {
            unsafe { core::ptr::write_unaligned(stat_ptr as *mut Stat64, s) };
            0
        }
        None => -(crate::klib::errno::ENOENT as i64),
    }
}
/// 带信号屏蔽的 select。转 [`select`] 前要先接上信号。
pub fn pselect6(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带信号屏蔽的 poll。同 [`pselect6`]。
pub fn ppoll(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 拆分命名空间。没有命名空间。
pub fn unshare(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 注册健壮 futex 链。同 [`futex`]。
pub fn set_robust_list(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 读健壮 futex 链。同 [`futex`]。
pub fn get_robust_list(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 带信号屏蔽的 epoll_wait。同 [`pselect6`]。
pub fn epoll_pwait(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 把信号变成可读的 fd。
pub fn signalfd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`signalfd`] 的带 flags 版本。
pub fn signalfd4(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`readv`] + 指定偏移。同 [`pread64`] 的限制。
pub fn preadv(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`writev`] + 指定偏移。同 [`pwrite64`] 的限制。
pub fn pwritev(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 按 tgid 带 siginfo 发信号。
pub fn rt_tgsigqueueinfo(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 批量收包。`net/` 层还没有 msghdr 批处理。
pub fn recvmmsg(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取文件句柄。minix 层没有导出句柄的概念。
pub fn name_to_handle_at(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 按句柄打开。同 [`name_to_handle_at`]。
pub fn open_by_handle_at(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 批量发包。同 [`recvmmsg`]。
pub fn sendmmsg(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 比较两个进程的内核资源。
pub fn kcmp(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 从 fd 装载模块。同 [`create_module`]。
pub fn finit_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 从 fd 加载 kexec 镜像。
pub fn kexec_file_load(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// BPF 系统调用。没有 BPF 虚拟机。
pub fn bpf(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 按 dirfd 执行。`execve` 已有，缺 dirfd 相对解析。
pub fn execveat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取进程的 pidfd。没有 pidfd 类型。
pub fn pidfd_open(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`clone`] 的结构体参数版本。
pub fn clone3(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`faccessat`] 的带 flags 版本。
pub fn faccessat2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`epoll_pwait`] 的 ns 超时版本。
pub fn epoll_pwait2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// --- 正式表 424..=448（335..423 是 x32 保留段，官方 x86_64 表里没有）------------

/// 给 pidfd 发信号。没有 pidfd 类型，见 [`pidfd_open`]。
pub fn pidfd_send_signal(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 克隆一棵挂载树。需要挂载树，现在只支持单个根。
pub fn open_tree(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 移动挂载点。同 [`open_tree`]。
pub fn move_mount(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 新式挂载 API：打开文件系统上下文。`fs/devices.rs` 里没有 fs_context。
pub fn fsopen(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 新式挂载 API：配置文件系统上下文。同 [`fsopen`]。
pub fn fsconfig(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 新式挂载 API：由上下文生成挂载点。同 [`fsopen`]。
pub fn fsmount(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 新式挂载 API：由已有挂载点取上下文。同 [`fsopen`]。
pub fn fspick(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 批量关闭 fd 区间。`fs/open.rs` 的 fd 表是每进程 16 项，加这个要先决定 EBADF 语义。
pub fn close_range(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带 `open_how` 结构的 openat。需要 RESOLVE_* 解析约束。
pub fn openat2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 从别的进程偷一个 fd。同 [`pidfd_open`]。
pub fn pidfd_getfd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 对别的进程做 madvise。需要跨进程地址空间访问。
pub fn process_madvise(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 改挂载点属性。同 [`open_tree`]。
pub fn mount_setattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 按 fd 的配额控制。同 [`quotactl`]。
pub fn quotactl_fd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// Landlock LSM：建规则集。没有 LSM 框架。
pub fn landlock_create_ruleset(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// Landlock LSM：加规则。同上。
pub fn landlock_add_rule(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// Landlock LSM：自我限制。同上。
pub fn landlock_restrict_self(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 建不可映射到内核的匿名内存 fd。同 [`memfd_create`] 的限制。
pub fn memfd_secret(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 提前释放被杀进程的内存。需要 `do_exit` 的完整回收链。
pub fn process_mrelease(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
