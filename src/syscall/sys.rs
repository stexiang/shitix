//! 系统调用的具体实现。对应 linux-1.0.9 的 `kernel/sys.c` 与
//! `kernel/sched.c` 里那些 `sys_*` 函数。
//!
//! 这里只实现不依赖 `fs/`、`kernel/signal.c`、`mm/mmap.c` 的那些，
//! 其余在分发表里指向 [`ni_syscall`]。每个函数的文档注明原版位置。

use super::{SysArgs, nr};
use crate::klib::errno::{EFAULT, EINVAL, ENOSYS, EBADF, EPERM, ERANGE, EINTR, ENOENT, ENODEV, EOPNOTSUPP, ESRCH, ENAMETOOLONG, EAGAIN};
use crate::klib::printk::Level;
use crate::sched;
use crate::traps::PtRegs;

/// 时间值结构
#[repr(C)]
#[derive(Clone, Copy)]
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

/// `struct itimerval`（x86_64：两个 `timeval`）。
#[repr(C)]
pub struct ItimerVal {
    pub it_interval: TimeVal,
    pub it_value: TimeVal,
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

/// 系统主机名。`sethostname(2)` 写入，`uname(2)` 的 nodename 读它。
/// 空（首字节 0）时 `uname` 回退到 [`crate::UTS_SYSNAME`]。
static mut HOSTNAME: [u8; 65] = [0; 65];
/// 系统域名。`setdomainname(2)` 写入，`uname(2)` 的 domainname 读它。
static mut DOMAINNAME: [u8; 65] = [0; 65];

/// 把一个 NUL 结尾的字节串拷进定长缓冲（越界截断，末尾补 NUL）。
/// 返回实际写入的字节数。
fn hostname_set(dst: &mut [u8], src: &[u8]) {
    let n = src.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&src[..n]);
    for b in &mut dst[n..] {
        *b = 0;
    }
}

/// 资源使用情况
#[repr(C)]
pub struct RUsage {
    pub ru_utime: TimeVal,
    pub ru_stime: TimeVal,
}

/// 资源限制
#[repr(C)]
#[derive(Clone, Copy)]
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
    if ptr == 0 {
        return false;
    }
    // 低 3GB：恒等映射段。内核 selftest 把内核 rodata 当「用户指针」传也走这里，
    // 必须保留（旧行为）；真实用户进程的低地址同样在此区间。
    if len <= IDENTITY_LIMIT && ptr.checked_add(len).is_some_and(|e| e <= IDENTITY_LIMIT) {
        return true;
    }
    // 高位地址（动态链接器 / 共享库被加载到 0x7f_... 等）不在恒等映射里，
    // 但内核态 CR3 == 当前进程 PML4，可以直接访问。这里查用户页表确认真的
    // 映射了（PRESENT|USER）。旧实现只看 3GB 上限，把 ld.so 的 .rodata 里那些
    // writev 的 iovec 字符串全挡成 -EFAULT，导致 glibc 错误信息只打出程序名。
    let pml4 = crate::mm::paging::current_pml4();
    crate::mm::area::verify_area(pml4, ptr, len, crate::mm::area::AccessMode::Read) == 0
}

/// 把用户传来的路径指针借成字节切片，顺带做 [`check_range`] 校验。
///
/// 长度上限取 `PATH_MAX`(4096)，避免坏指针上 `strlen` 一路扫到映射边界。
///
/// # Safety
/// 返回的切片只在本次系统调用期间使用；调用方不得让它逃出去。
/// 判断 `[ptr, ptr+len)` 在当前任务下是否可读/可写。
///
/// 低 1GB（恒等映射）直接放行——这是老 `check_range` 的语义：内核态静态字符串
/// （fs_init_thread 用 "init"/"/init" exec）和用户低段（堆、静态二进制）都覆盖在
/// 恒等映射里。0x7f_… 高位（动态链接器/共享库/父进程栈/argv/envp）走 `verify_area`
/// 查当前任务用户页表（顺带 resolve 惰性页）。
///
/// # Safety
/// 只用于当前任务的系统调用上下文（`sched::current()` 有效）。
unsafe fn user_ok(ptr: u64, len: u64, mode: crate::mm::area::AccessMode) -> bool {
    if check_range(ptr, len) {
        return true;
    }
    let pml4 = unsafe { (*crate::sched::task_ptr(crate::sched::current_index())).pml4 };
    crate::mm::area::verify_area(pml4, ptr, len, mode) == 0
}

unsafe fn user_path<'a>(ptr: u64) -> Result<&'a [u8], i64> {
    if !user_ok(ptr, 1, crate::mm::area::AccessMode::Read) {
        return Err(-(EFAULT as i64));
    }
    // SAFETY: user_ok 确认起始地址可读；strnlen 有上界，不会越过 4096 继续扫。
    let n = unsafe { crate::klib::string::strnlen(ptr as *const u8, 4096) };
    if n == 0 || n >= 4096 {
        return Err(-(EINVAL as i64));
    }
    if !user_ok(ptr, n as u64, crate::mm::area::AccessMode::Read) {
        return Err(-(EFAULT as i64));
    }
    // SAFETY: 同上，n 是刚量出来的长度，整段可读。
    Ok(unsafe { core::slice::from_raw_parts(ptr as *const u8, n) })
}

/// 把用户缓冲区借成可写切片，带 [`user_ok`] 校验。
///
/// # Safety
/// 同 [`user_path`]。
unsafe fn user_buf_mut<'a>(ptr: u64, len: u64) -> Result<&'a mut [u8], i64> {
    if len == 0 {
        return Ok(&mut []);
    }
    if !user_ok(ptr, len, crate::mm::area::AccessMode::Write) {
        return Err(-(EFAULT as i64));
    }
    // SAFETY: user_ok 确认整段可写（低 1GB 直过 / 高位查页表），当前 CR3 即该
    // 任务 pml4，直接解引用等价于按用户页表访问。
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) })
}

/// 把用户缓冲区借成只读切片，带 [`user_ok`] 校验。
///
/// # Safety
/// 同 [`user_path`]。
unsafe fn user_buf<'a>(ptr: u64, len: u64) -> Result<&'a [u8], i64> {
    if len == 0 {
        return Ok(&[]);
    }
    if !user_ok(ptr, len, crate::mm::area::AccessMode::Read) {
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
    use crate::klib::errno::{EINVAL, ENOENT, ENOMEM, ENOEXEC, ELOOP as ELOOP_ERR};
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

    // 2. 从文件系统打开并读取 ELF（通过 VFS namei → read）。
    // 支持 shebang（`#!interp [arg]`）：首两字节是 "#!" 时解析解释器行，
    // 改为 exec 解释器本身，原脚本路径进 argv（Linux fs/binfmt_script.c）。
    // 最多套 4 层（解释器本身也可能是脚本，如 busybox 的 #!/bin/sh）。
    let buf = crate::mm::get_free_page();
    if buf == 0 {
        return -(ENOMEM as i64);
    }
    // shebang 注入的内核侧字符串（解释器路径与可选参数），argv 构建时用
    let mut shebang_interp: Option<([u8; 128], usize)> = None;
    let mut shebang_arg: Option<([u8; 128], usize)> = None;
    let mut cur_path = [0u8; 128];
    cur_path[..path.len().min(128)].copy_from_slice(&path[..path.len().min(128)]);
    let mut cur_path_len = path.len().min(128);
    let mut fd: usize = 0;
    let mut n: i64 = 0;
    let mut header = None;
    for _depth in 0..4 {
        let f = unsafe { crate::fs::open::sys_open(&cur_path[..cur_path_len], crate::fs::oflags::O_RDONLY, 0) };
        if f < 0 {
            crate::mm::free_page(buf);
            return f;
        }
        fd = f as usize;
        let page_slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, crate::mm::PAGE_SIZE) };
        n = unsafe { crate::fs::read_write::read(fd, page_slice) };
        let data = unsafe { core::slice::from_raw_parts(buf as *const u8, n as usize) };
        if data.len() >= 2 && data[0] == b'#' && data[1] == b'!' {
            // 解析 "#!interp [arg]\n"
            let line_end = data.iter().position(|&c| c == b'\n').unwrap_or(data.len());
            let line = &data[2..line_end];
            // 跳过前导空白，取解释器路径（到空白为止），余下整体当一个参数
            let mut i = 0;
            while i < line.len() && (line[i] == b' ' || line[i] == b'\t') { i += 1; }
            let istart = i;
            while i < line.len() && line[i] != b' ' && line[i] != b'\t' { i += 1; }
            let ipath = &line[istart..i];
            while i < line.len() && (line[i] == b' ' || line[i] == b'\t') { i += 1; }
            let iarg = &line[i..];
            if ipath.is_empty() || ipath.len() > 127 {
                unsafe { crate::fs::open::sys_close(fd); }
                crate::mm::free_page(buf);
                return -(ENOEXEC as i64);
            }
            let mut ib = [0u8; 128];
            ib[..ipath.len()].copy_from_slice(ipath);
            shebang_interp = Some((ib, ipath.len()));
            if !iarg.is_empty() && iarg.len() <= 127 {
                let mut ab = [0u8; 128];
                ab[..iarg.len()].copy_from_slice(iarg);
                shebang_arg = Some((ab, iarg.len()));
            }
            unsafe { crate::fs::open::sys_close(fd); }
            cur_path = ib;
            cur_path_len = ipath.len();
            continue;
        }
        // 3. 解析 ELF64
        match parse_elf64(data) {
            Ok(h) if is_executable64(&h).is_ok() => { header = Some(h); break; }
            _ => {
                crate::mm::free_page(buf);
                unsafe { crate::fs::open::sys_close(fd); }
                return -(ENOEXEC as i64);
            }
        }
    }
    let header = match header {
        Some(h) => h,
        None => {
            // 套了 4 层还是脚本
            crate::mm::free_page(buf);
            return -(ELOOP_ERR as i64);
        }
    };
    let elf_data = unsafe { core::slice::from_raw_parts(buf as *const u8, n as usize) };

    // 3.5 PIE base
    let is_pie = header.e_type == 3;
    let pie_base: usize = if is_pie { 0x5555_0000 } else { 0 };

    // 4. 记录旧 PML4
    let old_pml4 = unsafe {
        let me = sched::task_ptr(sched::current_index());
        let old = (*me).pml4;
        (*me).pml4 = 0;
        (*me).tss.cr3 = 0;
        old
    };
    // 旧地址空间的 file-backed mmap VMA 记账全部作废（新程序重新 mmap）。
    crate::mm::mmap_vma::clear(sched::current_index());

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
            first_load_va = phdr.p_vaddr + pie_base as u64;
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

    // Add PIE base to AT_PHDR if from PT_PHDR
    if at_phdr != 0 && pie_base > 0 { at_phdr += pie_base as u64; }
    if pie_base > 0 { entry += pie_base as u64; }

    // 主程序入口（AT_ENTRY 用）。动态链接时下面会把 `entry` 覆盖成解释器
    // （ld.so）的入口，但 AT_ENTRY 必须是**主程序**的入口——ld.so 加载完所有
    // 库后要跳到这里。旧代码用同一个 `entry` 变量，AT_ENTRY 被写成了 ld.so
    // 自己的入口，ld.so 完成重定位后跳回自己 → 循环/错乱（表现为 stderr 只
    // 打出程序名 "/init" 就退 1）。
    let main_entry = entry;

    // 6a. 如果有 PT_INTERP，加载动态链接器
    const INTERP_PIE_BASE: usize = 0x7f_0000_0000;
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
                            let ipie_base: usize = if ihdr.e_type == 3 { INTERP_PIE_BASE } else { 0 };
                            let iphoff = ihdr.e_phoff as usize;
                            // 加载解释器的 PT_LOAD 段
                            for j in 0..ihdr.e_phnum as usize {
                                let iphd = match parse_phdr64(idata, iphoff + j * ihdr.e_phentsize as usize) {
                                    Ok(p) => p,
                                    Err(_) => continue,
                                };
                                if iphd.p_type != ElfPType::Load as u32 { continue; }
                                let ivaddr = iphd.p_vaddr as usize + ipie_base;
                                let ifilesz = iphd.p_filesz as usize;
                                let imemsz = iphd.p_memsz as usize;
                                let ifoff = iphd.p_offset as usize;
                                let iprot = crate::elf::phdr_prot_to_flags(iphd.p_flags);
                                let istart = ivaddr & !0xFFF;
                                let iend = page_align(ivaddr + imemsz);
                                for va in (istart..iend).step_by(PAGE_SIZE) {
                                    let pg = get_free_page();
                                    if pg == 0 { break; }
                                    if !unsafe { paging::map_page(new_pml4, va, pg, iprot) } {
                                        free_page(pg); break;
                                    }
                                    // 从解释器**文件**读入段内容（不是从 ibuf 那一页缓冲）。
                                    // 旧实现只用 read(ifd) 读了一页进 ibuf，再按 `ifoff + ...`
                                    // 从 ibuf 拷——只要段在文件里的偏移 >= 一页（比如 glibc 的
                                    // ld-linux 把 .text 放在 offset 0x1000），条件
                                    // `ifoff+.. <= idata.len()` 就恒不成立，整段文本被装成
                                    // 零页，ld.so 从入口开始跑的全是 0x00 字节。
                                    // 这里对齐主程序加载器（第 8 步）的做法：lseek + 循环 read。
                                    if va < ivaddr + ifilesz && va + PAGE_SIZE > ivaddr {
                                        let copy_start = if va < ivaddr { ivaddr - va } else { 0 };
                                        let copy_end = core::cmp::min(PAGE_SIZE, ivaddr + ifilesz - va);
                                        let copy_len = copy_end - copy_start;
                                        if copy_len > 0 {
                                            let file_src = ifoff + (va - ivaddr) + copy_start;
                                            unsafe {
                                                crate::fs::read_write::lseek(ifd as usize, file_src as i64, crate::fs::SEEK_SET);
                                                let dest = core::slice::from_raw_parts_mut(
                                                    (pg as *mut u8).add(copy_start), copy_len);
                                                let mut filled = 0usize;
                                                while filled < copy_len {
                                                    let n = crate::fs::read_write::read(ifd as usize, &mut dest[filled..]);
                                                    if n <= 0 { break; }
                                                    filled += n as usize;
                                                }
                                            }
                                        }
                                    }
                                }
                                if interp_base == 0 { interp_base = istart as u64; }
                            }
                            // entry = 解释器入口 (relocated)
                            entry = ihdr.e_entry + ipie_base as u64;
                        }
                    }
                }
                free_page(ibuf);
            }
            unsafe { crate::fs::open::sys_close(ifd as usize); }
        }
    }

    // 7. (第 0 页映射已移除：恢复 NULL 保护。)

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

        let vaddr = phdr.p_vaddr as usize + pie_base;
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

    // 7.5 Initialize brk with first page mapped
    let brk_va = page_align(max_va);
    let brk_page = get_free_page();
    if brk_page != 0 {
        unsafe { paging::map_page(new_pml4, brk_va, brk_page,
            paging::flags::USER | paging::flags::PRESENT | paging::flags::RW); }
    }
    unsafe {
        let nr = sched::current_index();
        let t = sched::task_ptr(nr);
        (*t).brk = brk_va + PAGE_SIZE;
    }

    // 8. 设置用户栈 —— glibc 启动（TLS 设置、IFUNC 解析、signal stack）需要较大栈，
    //    只给一页会溢出。glibc 动态 bash 实测会用到 ~67KB（超过原 64KB），
    //    这里给 2MB（512 页），对齐 Linux 默认栈大小量级，留足余量。
    const STACK_PAGES: usize = 512;
    let stack_top_off = STACK_PAGES * PAGE_SIZE;          // 栈区大小
    // 栈必须放在**高位地址**，远离数据段/堆（brk 在 max_va 之后向上长）。
    // 旧实现 `max(max_va + 0x10000, ...)` 把栈正好放在数据段之后——而 brk
    // 堆也从那里向上长，malloc 一旦 sbrk 就把堆顶进栈区，setup_frame 往栈
    // 上写的信号帧蹦床字节（mov $15,%rax; syscall）直接砸进 malloc 出的
    // WORD_LIST.next，变成 glibc 读到「代码字节」的野指针（0xf0000000...）。
    let stack_base_va = 0x7FFF_FF00_0000usize;              // 高位规范地址，远离堆与 mmap
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
    //   [字符串区：AT_RANDOM / AT_PLATFORM / argv[*] / envp[*] / AT_EXECFN]  ← 页最顶
    //   [auxv: AT_NULL..AT_*]
    //   [envp: ...NULL]
    //   [argv: ...NULL]
    //   [argc]
    // 早先这里把 argv 硬编码成 argc=1/argv[0]="/bin/sh"，导致 cat 等程序拿不到
    // 命令行参数（永远只读 stdin）。现在从用户空间读真正的 argv/envp。
    let n_slots = PAGE_SIZE / 8;
    let argc_slot: isize;
    let execfn_va: u64;
    let platform_va: u64;
    let argv0_va: u64;
    let random_va: u64;
    // 收集 argv/envp 字符串的虚拟地址（最多各 64 项）。
    const MAX_ARGS: usize = 64;
    let mut argv_vas: [u64; MAX_ARGS] = [0; MAX_ARGS];
    let mut envp_vas: [u64; MAX_ARGS] = [0; MAX_ARGS];
    let mut argc: usize = 0;
    let mut envc: usize = 0;
    // SAFETY: top_page 在恒等映射内，独占。
    unsafe {
        let base = top_page as *mut u8;
        let top = base.add(PAGE_SIZE); // 顶页末尾（exclusive）
        // 1) 字符串区：用递减游标从页顶往下放，互不重叠。
        let mut cur = top;
        // AT_RANDOM: 16 字节，glibc 期望 16 字节对齐，先对齐游标。
        cur = cur.sub((cur as usize) & 0xF);
        let rp = cur.sub(16);
        // 之前 write_bytes(rp,0,16) 全 0：glibc 用 AT_RANDOM 前 8 字节当栈金丝雀、
        // 后 8 字节当 pointer_guard(%fs:0x30)，全 0 会让 PTR_DEMANGLE/金丝雀形同虚设。
        // 用与 getrandom 相同的 rdtsc + xorshift 生成非零随机字节。
        let mut lo: u32 = 0; let mut hi: u32 = 0;
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
        static mut AT_RAND_CTR: u64 = 0x9E3779B97F4A7C15;
        let mut s = ((hi as u64) << 32 | lo as u64).wrapping_add(*core::ptr::addr_of!(AT_RAND_CTR));
        *core::ptr::addr_of_mut!(AT_RAND_CTR) = (*core::ptr::addr_of!(AT_RAND_CTR)).wrapping_add(0x9E3779B97F4A7C15);
        for i in 0..16 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            *rp.add(i) = (s >> 32) as u8;
        }
        random_va = (stack_top_va - (top as usize - rp as usize)) as u64;
        cur = rp;
        // 把一条用户字符串拷到栈字符串区，返回它的虚拟地址。
        // 失败（坏指针）时跳过该项（写一个空串占位），不致命。
        let mut put_user = |uptr: u64, cur: &mut *mut u8| -> u64 {
            // argv/envp 字符串在 0x7f_… 高位（父进程栈/堆），filename 在低段内核
            // 静态字符串——两者都靠 user_ok（低 1GB 直过 + 高位查页表）判定。
            let readable = uptr != 0 && user_ok(uptr, 1, crate::mm::area::AccessMode::Read);
            if !readable {
                // 空串占位
                let p = cur.sub(1);
                *p = 0;
                *cur = p;
                return (stack_top_va - (top as usize - p as usize)) as u64;
            }
            // 量长度（上限 4096，防坏指针）
            let n = crate::klib::string::strnlen(uptr as *const u8, 4096);
            let len = if n >= 4096 { 0 } else { n + 1 };
            if len == 0 {
                let p = cur.sub(1);
                *p = 0;
                *cur = p;
                return (stack_top_va - (top as usize - p as usize)) as u64;
            }
            // 对齐：保证后续 8 字节槽位区从 8 对齐开始（字符串区任意对齐都行，
            // 但 aux_top 算法要求字符串区结束后能 &!7）。
            let p = cur.sub(len);
            if user_ok(uptr, n as u64, crate::mm::area::AccessMode::Read) {
                core::ptr::copy_nonoverlapping(uptr as *const u8, p, n);
            }
            *p.add(n) = 0;
            *cur = p;
            (stack_top_va - (top as usize - p as usize)) as u64
        };
        // 普通定长字符串（AT_PLATFORM）
        let mut put_const = |s: &[u8], cur: &mut *mut u8| -> u64 {
            let len = s.len() + 1;
            let p = cur.sub(len);
            core::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
            *p.add(s.len()) = 0;
            *cur = p;
            (stack_top_va - (top as usize - p as usize)) as u64
        };
        platform_va = put_const(b"x86_64", &mut cur);

        // argv：args.a1 是 char*[]（NULL 终止）。逐项拷字符串。
        // shebang（#!/interp [arg]）时按 Linux binfmt_script 的语义重建 argv：
        // [interp, arg?, script_path, 原 argv[1..]]。
        let argv_arr = args.a1;
        let mut orig_idx: u64 = 0;
        if let Some((ib, ilen)) = shebang_interp {
            if argc < MAX_ARGS { argv_vas[argc] = put_const(&ib[..ilen], &mut cur); argc += 1; }
            if let Some((ab, alen)) = shebang_arg {
                if argc < MAX_ARGS { argv_vas[argc] = put_const(&ab[..alen], &mut cur); argc += 1; }
            }
            // 脚本路径（execve 的第一个参数）作为解释器的实参
            if argc < MAX_ARGS { argv_vas[argc] = put_user(filename_ptr, &mut cur); argc += 1; }
            orig_idx = 1; // 原 argv[0]（=脚本路径）已由 filename_ptr 提供
        }
        if argv_arr != 0 {
            loop {
                if argc >= MAX_ARGS { break; }
                // 指针数组本身在 0x7f_… 高位（父进程栈），check_range 1GB 会判无效
                // 导致 argc=0；用 user_ok（低段直过 + 高位查页表）。
                if !user_ok(argv_arr + orig_idx * 8, 8, crate::mm::area::AccessMode::Read) { break; }
                let p = core::ptr::read_volatile((argv_arr + orig_idx * 8) as *const u64);
                if p == 0 { break; } // NULL 终止
                let va = put_user(p, &mut cur);
                argv_vas[argc] = va;
                argc += 1;
                orig_idx += 1;
            }
        }
        // argv[0] 可能为空（execve 无 argv）→ 用 filename 补 argv[0]
        if argc == 0 {
            let va = put_user(filename_ptr, &mut cur);
            argv_vas[0] = va;
            argc = 1;
        }
        argv0_va = argv_vas[0];

        // envp：args.a2 是 char*[]（NULL 终止）。
        let envp_arr = args.a2;
        if envp_arr != 0 {
            loop {
                if envc >= MAX_ARGS { break; }
                if !user_ok(envp_arr + (envc as u64) * 8, 8, crate::mm::area::AccessMode::Read) { break; }
                let p = core::ptr::read_volatile((envp_arr + (envc as u64) * 8) as *const u64);
                if p == 0 { break; }
                let va = put_user(p, &mut cur);
                envp_vas[envc] = va;
                envc += 1;
            }
        }
        // AT_EXECFN：用 exec 的 filename（用户可见的程序路径）
        execfn_va = put_user(filename_ptr, &mut cur);

        // 2) auxv/argv/argc 槽位区：从字符串区下方往下写。
        // 16 字节对齐 aux_top：这样最高槽号 i 恒为奇数（见下），配合「必要时
        // 顶部垫一槽」能让 argc 最终落在偶数槽 → 入口 rsp 16 字节对齐。
        let aux_top = (cur as usize) & !0xF;
        // i 用 isize：argv/envp 很多时（gcc collect2 有 55~59 个参数），槽位区会
        // 从顶页往下溢出到下一页。此时 i 会变负，旧实现 `top_page as *mut u64` 的
        // `s.add(i)` 直接当物理地址用，负偏移会写到顶页下方 64KB 开外的垃圾地址，
        // argv 数组读出来全是 NULL。改用 translate 按虚拟地址逐槽求物理地址。
        let mut i: isize = ((aux_top - (top_page as usize)) / 8 - 1) as isize; // 最高可用槽
        let slot = |idx: isize| -> *mut u64 {
            let va = (stack_top_va as isize - PAGE_SIZE as isize) + idx * 8;
            unsafe { paging::translate(new_pml4, va as usize) }.unwrap_or(0) as *mut u64
        };
        // SysV ABI：入口 %rsp 必须 16 字节对齐，而 %rsp 指向 argc 槽，其地址
        // = top_page + argc_slot*8（top_page 页对齐），故 argc_slot 必须为偶数。
        // 总槽数 = 顶部填充(0/1) + auxv36 + envp_NULL1 + envc + argv_NULL1
        //          + argc + argc1 = (P + 39 + envc + argc)。i 恒奇，要 argc_slot
        // 为偶需 (envc+argc+P) 为奇：envc+argc 为偶时 P=1，为奇时 P=0。
        // 不垫这一槽，glibc ld.so 的 _dl_start 里 `movaps %xmm0,-0x70(%rbp)`
        // 会因 rsp 未对齐触发 #GP。
        if (envc + argc) & 1 == 0 {
            slot(i).write_volatile(0); i -= 1; // 顶部填充槽（不被引用）
        }
        slot(i).write_volatile(0); i -= 1; // AT_NULL val
        slot(i).write_volatile(0); i -= 1; // AT_NULL key
        slot(i).write_volatile(0); i -= 1; // AT_HWCAP2(26) val
        slot(i).write_volatile(26); i -= 1; // AT_HWCAP2 key
        slot(i).write_volatile(0xbfebfbff | (1<<0) | (1<<9) | (1<<19)); i -= 1; // AT_HWCAP(16) val (SSE/SSE2/etc)
        slot(i).write_volatile(16); i -= 1; // AT_HWCAP key
        slot(i).write_volatile(100); i -= 1; // AT_CLKTCK(17) val
        slot(i).write_volatile(17); i -= 1; // AT_CLKTCK key
        slot(i).write_volatile(random_va); i -= 1; // AT_RANDOM(25) val
        slot(i).write_volatile(25); i -= 1; // AT_RANDOM key
        slot(i).write_volatile(platform_va); i -= 1; // AT_PLATFORM(15) val
        slot(i).write_volatile(15); i -= 1; // AT_PLATFORM key
        slot(i).write_volatile(execfn_va); i -= 1; // AT_EXECFN(31) val
        slot(i).write_volatile(31); i -= 1; // AT_EXECFN key
        slot(i).write_volatile(main_entry); i -= 1; // AT_ENTRY(9) val（主程序入口）
        slot(i).write_volatile(9); i -= 1;    // AT_ENTRY key
        slot(i).write_volatile(interp_base); i -= 1; // AT_BASE(7) val
        slot(i).write_volatile(7); i -= 1;    // AT_BASE key
        slot(i).write_volatile(4096); i -= 1; // AT_PAGESZ(6) val
        slot(i).write_volatile(6); i -= 1;    // AT_PAGESZ key
        slot(i).write_volatile(phnum as u64); i -= 1; // AT_PHNUM(5) val
        slot(i).write_volatile(5); i -= 1;    // AT_PHNUM key
        slot(i).write_volatile(56); i -= 1;   // AT_PHENT(4) val
        slot(i).write_volatile(4); i -= 1;    // AT_PHENT key
        slot(i).write_volatile(at_phdr); i -= 1; // AT_PHDR(3) val
        slot(i).write_volatile(3); i -= 1;    // AT_PHDR key
        slot(i).write_volatile(0); i -= 1;    // AT_EGID(14) val
        slot(i).write_volatile(14); i -= 1;   // AT_EGID key
        slot(i).write_volatile(0); i -= 1;    // AT_GID(13) val
        slot(i).write_volatile(13); i -= 1;   // AT_GID key
        slot(i).write_volatile(0); i -= 1;    // AT_EUID(12) val
        slot(i).write_volatile(12); i -= 1;   // AT_EUID key
        slot(i).write_volatile(0); i -= 1;    // AT_UID(11) val
        slot(i).write_volatile(11); i -= 1;   // AT_UID key
        slot(i).write_volatile(0); i -= 1;    // AT_SECURE(23) val
        slot(i).write_volatile(23); i -= 1;   // AT_SECURE key
        // envp 终止 NULL
        slot(i).write_volatile(0); i -= 1;
        // envp[envc-1 .. 0]（逆序写）
        for k in (0..envc).rev() {
            slot(i).write_volatile(envp_vas[k]); i -= 1;
        }
        // argv 终止 NULL
        slot(i).write_volatile(0); i -= 1;
        // argv[argc-1 .. 0]（逆序写）
        for k in (0..argc).rev() {
            slot(i).write_volatile(argv_vas[k]); i -= 1;
        }
        // argc
        slot(i).write_volatile(argc as u64);
        argc_slot = i;
    }
    // user_rsp 指向 argc 槽的虚拟地址。
    let user_rsp = stack_top_va as u64 - ((n_slots as isize - argc_slot) * 8) as u64;

    // 8. 设置当前任务使用新页表，并立即加载 CR3。
    unsafe {
        let me = sched::task_ptr(sched::current_index());
        (*me).pml4 = new_pml4;
        (*me).tss.cr3 = new_pml4 as u64;
        // execve 不会切任务，所以 switch_to_task 不会替我们换 CR3——
        // 必须在这里立即加载，否则 iretq 后 CPU 还在用旧的（可能是 boot）CR3。
        core::arch::asm!("mov cr3, {}", in(reg) new_pml4 as u64, options(preserves_flags));
        // CR3 已切换到新 PML4，现在安全释放旧 PML4 及其用户页。
        // VFORK 子进程的 old_pml4 是与被挂起父进程共享的页表，不能 free
        // （父进程 resume 后还要用它），留给父进程。
        if old_pml4 != 0 && old_pml4 != new_pml4 && (*me).vfork_parent == 0 {
            unsafe { crate::mm::paging::free_user_pages(old_pml4) };
            crate::mm::free_page(old_pml4);
        }
        // VFORK：子进程已完成 exec，唤醒被挂起的父进程。
        if (*me).vfork_parent != 0 {
            let vp = (*me).vfork_parent;
            (*me).vfork_parent = 0;
            (*sched::task_ptr(vp)).state = crate::sched::task::TaskState::Running;
        }
        // 清掉 TLS：新程序从「无 TLS」状态开始，父进程遗留的 fs_base/gs_base
        // 指向已随 exec 失效的旧地址空间，留着会让新程序在第一次 %fs 访问
        // （glibc 启动早期）就 #GP。arch_prctl(ARCH_SET_FS) 会在新 TLS 建立
        // 时重新写入。同步写 MSR，因为本次 execve 不经过 switch_to_task。
        (*me).fs_base = 0;
        (*me).gs_base = 0;
        core::arch::asm!("wrmsr",
            in("ecx") 0xC000_0100u64, in("eax") 0u32, in("edx") 0u32,
            options(nomem, nostack, preserves_flags));
        core::arch::asm!("wrmsr",
            in("ecx") 0xC000_0101u64, in("eax") 0u32, in("edx") 0u32,
            options(nomem, nostack, preserves_flags));
    }

    // 8.5 关闭 close-on-exec 的 fd（FD_CLOEXEC，open 的 O_CLOEXEC 或
    // fcntl F_SETFD 设置）。原版 do_execve → flush_old_exec → close_files
    // 按位图逐个 sys_close。
    {
        let nr = sched::current_index();
        let coe = unsafe { (*sched::task_ptr(nr)).close_on_exec };
        if coe != 0 {
            for fd in 0..crate::fs::NR_OPEN {
                if coe & (1u64 << fd) != 0 {
                    close_one_fd(fd);
                }
            }
            unsafe { (*sched::task_ptr(nr)).close_on_exec = 0 };
        }
    }

    // 9. 改写 pt_regs：下次 iretq 到新程序入口
    regs.rip = entry;
    regs.rsp = user_rsp;
    regs.cs = USER_CS as u64;
    regs.rflags = 0x202;
    regs.ss = USER_DS as u64;
    regs.rax = 0;
    // rdx 是 SysV ABI 的「rtld_fini」寄存器：静态二进制 _start 会把入口时的
    // rdx 当 atexit 函数指针传给 __libc_start_main。内核发起的 execve（LFS boot
    // 用 syscall3(EXECVE,path,0,envp)）会把 rdx 留成 envp 的内核栈地址，静态
    // glibc 注册成退出处理函数，exit 时 call *%rax 跳进内核栈地址 → #PF。
    // Linux 在 start_thread 里显式清 dx，这里只清 rdx（最小修复）；其余参数
    // 寄存器按 ABI 属于 undefined，动态链接器 ld.so 不读它们。
    regs.rdx = 0;

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
    // 匿名事件 fd（仅 eventfd 可写）。
    if crate::fs::event::fd_is_event(fd as usize) {
        return crate::fs::event::write(fd as usize, args.a1, sz64);
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
    // 对齐 Linux：sys_exit 把退出码编成最终状态字（高 8 位退出码、低 7 位
    // 信号为 0），do_exit 收到的是「已编码的退出状态」；信号终止则直接传
    // 原始信号号（1..=31，落在低 7 位）。这样 wait4 无需再猜 1..=31 是
    // 退出码还是信号——之前 `/bin/false`(exit 1) 会被误报成 SIGHUP。
    crate::exit::do_exit((args.a0 as i32 & 0xff) << 8)
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
        // nodename：sethostname 设置过就用它，否则回退 sysname。
        let nodename = {
            let h = &*core::ptr::addr_of!(HOSTNAME);
            let n = h.iter().position(|&b| b == 0).unwrap_or(65);
            if n > 0 {
                core::str::from_utf8(&h[..n]).unwrap_or(crate::UTS_SYSNAME)
            } else {
                crate::UTS_SYSNAME
            }
        };
        fill(p, nodename, utsname_len);
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
        // domainname（GNU 扩展，第 6 字段）：setdomainname 设置过就用它。
        let domainname = {
            let d = &*core::ptr::addr_of!(DOMAINNAME);
            let n = d.iter().position(|&b| b == 0).unwrap_or(65);
            if n > 0 {
                core::str::from_utf8(&d[..n]).unwrap_or("")
            } else {
                ""
            }
        };
        fill(p, domainname, utsname_len);
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

        // RLIMIT_DATA 强制（原版 do_brk→check_data_rlimit 语义）：
        // 数据段（堆）增长不能超过软限。
        if new_brk > cur_brk {
            let rlim = (*t).rlim[sched::task::RLIMIT_DATA];
            if rlim.rlim_cur != sched::task::RLIM_INFINITY {
                let grow = (new_brk - cur_brk) as u64;
                if grow > rlim.rlim_cur {
                    return -(crate::klib::errno::ENOMEM as i64);
                }
            }
        }

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
                if !paging::is_user_mapped(pml4, va) && !paging::is_reserved(pml4, va) {
                    // 惰性分配：堆页先保留，等首次访问再由 page fault 落实物理页。
                    if !paging::map_reserved(pml4, va, paging::flags::SHARED) {
                        // 失败必须返回负 errno（ENOMEM），不能返回 old_brk 这个
                        // 正地址——glibc 的 sbrk 靠 `brk()<0` 判失败，返回旧
                        // 断点会被当成成功，__curbrk 被推进到一个没映射的地址。
                        return -(crate::klib::errno::ENOMEM as i64);
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
    // 匿名事件 fd（eventfd/timerfd/signalfd/...）fast path。
    if crate::fs::event::fd_is_event(fd as usize) {
        // SAFETY: 调用方保证 buf 可写；event.rs 内按 len 校验。
        return crate::fs::event::read(fd as usize, args.a1, sz64);
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
/// inotify 钩子：广播「path 最后一级发生了 mask 事件」。
/// 目录取最后一个 '/' 之前（无前缀视作 "."、根视作 "/"）。
fn notify_path_event(path: &[u8], mask: u32) {
    let n = path.len().min(255);
    if n == 0 { return; }
    let full = &path[..n];
    let (dir, name): (&[u8], &[u8]) = match full.iter().rposition(|&c| c == b'/') {
        None => (b".", full),
        Some(0) => (b"/", &full[1..]),
        Some(s) => (&full[..s], &full[s + 1..]),
    };
    crate::fs::event::inotify_notify(dir, mask, name, 0);
}

pub fn open(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: user_path 已校验范围；fs 层自己处理不存在/权限。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // O_CLOEXEC 是「打开后设 FD_CLOEXEC」的约定，不是文件打开语义，
    // 剥掉再往下传（fs 层不认识它）。
    let flags = args.a1 as u32;
    let cloexec = flags & crate::fs::oflags::O_CLOEXEC != 0;
    // SAFETY: 系统调用上下文，fs 层会睡（getblk/wait_on_buffer），
    // 所以只能在有 current 的任务里调——系统调用天然满足。
    let fd = unsafe { crate::fs::open::sys_open(path, flags & !crate::fs::oflags::O_CLOEXEC, args.a2 as u16) };
    if fd >= 0 && flags & crate::fs::oflags::O_CREAT != 0 {
        // 近似：无法区分「新建」与「打开已存在」，O_CREAT 成功即报 IN_CREATE
        notify_path_event(path, crate::fs::event::IN_CREATE);
    }
    if fd >= 0 && cloexec {
        let nr = sched::current_index();
        unsafe { (*sched::task_ptr(nr)).close_on_exec |= 1u64 << (fd as usize & 63) };
    }
    fd
}

/// 关闭文件。对应原版 `fs/open.c:sys_close()`。
pub fn close(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    close_one_fd(fd as usize)
}

/// 关闭单个 fd 的全部类型分支（管道/socket/event/普通文件）。
/// 给 [`close`] 与 [`close_range`] 共用；未打开的 fd 静默返回 0（close_range 语义），
/// 单发 close() 需要的 EBADF 由 open::sys_close 自己返回。
fn close_one_fd(fd: usize) -> i64 {
    let mut closed = false;
    // Pipe cleanup（按方向递减计数，两端归零才释放）
    if crate::fs::pipe::fd_is_pipe(fd) {
        crate::fs::pipe::close_fd(fd);
        closed = true;
    }
    // Socket cleanup
    if crate::net::socket::fd_is_socket(fd) {
        crate::net::socket::close_socket(fd);
        closed = true;
    }
    // 匿名事件 fd cleanup
    if crate::fs::event::fd_is_event(fd) {
        crate::fs::event::close(fd);
        closed = true;
    }
    if closed {
        // 管道/socket fd 已关，返回 0。之前这里落到 sys_close 会返回 -EBADF，
        // 让 glibc 误以为 fd 无效（bash 管道的 fd 清理依赖 close 返回 0）。
        return 0;
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

    // RLIMIT_AS 强制（原版 do_mmap→may_expand_vm 语义）：
    // 单次映射长度不能超过剩余虚拟地址空间软限。没有完整 VMA 总量统计，
    // 用「请求长度 ≤ 软限」近似——足够拦住 glibc malloc 那种 ~1.4GB 预留
    // 被限小后仍返回成功导致越界写的情况。
    {
        let rlim = unsafe { (*sched::task_ptr(sched::current_index())).rlim[sched::task::RLIMIT_AS] };
        if rlim.rlim_cur != sched::task::RLIM_INFINITY && len > rlim.rlim_cur {
            return -(ENOMEM as i64);
        }
    }

    const MAP_ANONYMOUS: u64 = 0x20;
    const MAP_PRIVATE: u64 = 0x02;
    const PROT_WRITE: u64 = 0x02;

    unsafe {
        let nr = sched::current_index();
        let t = sched::task_ptr(nr);
        let pml4 = (*t).pml4;
        // Kernel threads have pml4=0; use boot PML4 (0x4000) for them
        let pml4 = if pml4 == 0 { 0x4000usize } else { pml4 };

        let map_addr = if addr != 0 {
            (addr as usize) & !0xFFF
        } else {
            let base = (*t).mmap_base;
            let alloc_end = if base < len { 0usize } else { (base - len as u64) as usize & !0xFFF };
            if alloc_end == 0 { return -(ENOMEM as i64); }
            (*t).mmap_base = alloc_end as u64;
            alloc_end
        };
        let npages = ((len as usize) + crate::mm::PAGE_SIZE - 1) / crate::mm::PAGE_SIZE;
        let mut pg_flags = paging::flags::USER | paging::flags::PRESENT;
        if prot & PROT_WRITE != 0 { pg_flags |= paging::flags::RW; }

        // MAP_FIXED（addr != 0）：Linux 语义是先清掉目标区间的旧映射
        // 再覆盖。glibc 动态链接器对共享库就是「整文件先 map 一次再逐段
        // MAP_FIXED 重 map」，不清旧 VMA 会被 VMA 重叠检查拒绝成 ENOMEM。
        if addr != 0 {
            let task = sched::current_index();
            crate::mm::mmap_vma::remove_range(
                task, map_addr, map_addr + npages * crate::mm::PAGE_SIZE,
            );
        }
        // File-backed MAP_PRIVATE: 惰性映射——只建 RESERVED 叶子 + 记 VMA，
        // 文件内容等第一次访问时由 page fault 路径按 (sb, ino, offset) 读入。
        // 旧实现 eager 逐页 read：glibc 动态链接器 map libc 数 MB 文本，
        // 进程实际只触碰一小部分，eager 既慢又白占物理页。
        if flags & MAP_ANONYMOUS == 0 && flags & MAP_PRIVATE != 0 {
            let fd = fd as i64;
            if fd < 0 { return -(EBADF as i64); }
            let f = crate::fs::open::fd_to_filp(fd as usize);
            if f == crate::fs::inode::NIL { return -(EBADF as i64); }
            let ino = unsafe { crate::fs::file_table::filp(f).f_inode };
            if ino == crate::fs::inode::NIL { return -(EBADF as i64); }
            let (i_ino, i_sb) = unsafe {
                let i = crate::fs::inode::inode(ino);
                (i.i_ino, i.i_sb)
            };
            if i_sb == crate::fs::inode::NIL { return -(EBADF as i64); }

            for i in 0..npages {
                let va = map_addr + i * crate::mm::PAGE_SIZE;
                if !unsafe { paging::map_reserved(pml4, va, pg_flags) } {
                    crate::pr_warn!("mmap: map_reserved fail va={:#x} len={}", va, len);
                    return -(ENOMEM as i64);
                }
            }
            let task = sched::current_index();
            let vma = crate::mm::mmap_vma::MmapVma {
                start: map_addr,
                end: map_addr + npages * crate::mm::PAGE_SIZE,
                prot: pg_flags,
                kind: crate::mm::mmap_vma::VmaKind::File {
                    sb: i_sb,
                    ino: i_ino,
                    offset,
                },
            };
            if !crate::mm::mmap_vma::add(task, vma) {
                // 记账失败（表满/重叠）：回滚保留页，别让 VMA 缺失的
                // reserved 页留在页表里——缺页时会落成匿名零页，mmap
                // 语义悄悄错了比 ENOMEM 更难查。
                for i in 0..npages {
                    let va = map_addr + i * crate::mm::PAGE_SIZE;
                    unsafe { paging::unmap_page(pml4, va) };
                }
                crate::pr_warn!("mmap: vma add fail va={:#x} npages={} task={} nrvma={}",
                    map_addr, npages, task, crate::mm::mmap_vma::count(task));
                return -(ENOMEM as i64);
            }
            return map_addr as i64;
        }

        // MAP_ANONYMOUS: 惰性分配（先保留虚拟空间，物理页等首次访问时
        // 由 page fault 的 resolve_reserved 再落实）。glibc 初始化 malloc
        // 主竞技场会一次 mmap ~1.4GB，若这里每页都立刻 get_free_page，
        // 256MB 内存直接吃光。
        if flags & MAP_ANONYMOUS == 0 { return -(ENOSYS as i64); }

        for i in 0..npages {
            let va = map_addr + i * crate::mm::PAGE_SIZE;
            if !unsafe { paging::map_reserved(pml4, va, pg_flags) } {
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
            } else {
                // RESERVED（惰性）页 translate 不到物理地址，也要把叶子摘掉
                paging::unmap_page(pml4, va);
            }
        }
        // 摘掉覆盖区间的 file-backed VMA 记账
        crate::mm::mmap_vma::remove_range(nr, addr, addr + npages * crate::mm::PAGE_SIZE);
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
            if !paging::set_page_flags(pml4, va, new_flags) {
                // 非 PRESENT 叶子：惰性保留页（RESERVED）/已换出页
                // （SWAPPED）也要更新权限位，否则 glibc 对还没触碰过的
                // RELRO 段 mprotect(PROT_READ) 会静默丢失，之后缺页
                // 落实时又按旧 prot 落回可写。物理地址/槽号位原样保留。
                if let Some(leaf) = paging::leaf_entry(pml4, va) {
                    if leaf & (paging::flags::RESERVED | paging::flags::SWAPPED) != 0 {
                        let keep = leaf & !(paging::flags::RW | paging::flags::USER
                            | paging::flags::NO_EXEC);
                        paging::set_leaf_entry(pml4, va, keep | new_flags);
                    }
                }
            }
            va += PAGE_SIZE;
        }
        // 同步更新被整段覆盖的 file-backed VMA 的 prot（部分覆盖的
        // 不拆 VMA——叶子权限已经改对，VMA prot 只影响尚未触碰的
        // 惰性页，少见，按保守处理跳过）。
        let prot_bits = new_flags & (paging::flags::RW | paging::flags::USER
            | paging::flags::NO_EXEC);
        crate::mm::mmap_vma::set_prot(sched::current_index(), start, end, prot_bits);
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
    
    if !unsafe { user_ok(args.a0, size as u64, crate::mm::area::AccessMode::Write) } {
        return -(EFAULT as i64);
    }

    // 真实回溯：从 pwd 出发，反复在父目录里按 inode 号反查自己的名字，
    // 直到 task 的 root（chroot 边界）。对应现代内核 prepend_path 沿
    // d_parent 走的语义；本树没有 dcache，用「扫父目录」代替。
    // SAFETY: 系统调用上下文。
    unsafe {
        let nr = sched::current_index();
        let t = sched::task_ptr(nr);
        let pwd = (*t).pwd;
        let root = (*t).root;
        if pwd == usize::MAX {
            return -(ENOENT as i64);
        }
        // 分量栈（自叶向根收集，输出时倒序）。深度封顶 32 层防路径死循环。
        let mut comps: [([u8; 255], usize); 32] = [([0; 255], 0); 32];
        let mut depth = 0usize;
        let mut cur = pwd;
        // 非 pwd/root 的中间 inode 是我们 lookup_one("..") 多拿的引用，用完要还。
        while cur != root {
            let cur_ino = crate::fs::inode::inode(cur).i_ino;
            let parent = match crate::fs::namei::lookup_one(cur, b"..") {
                Ok(p) => p,
                Err(e) => {
                    if cur != pwd { crate::fs::inode::iput(cur); }
                    return -(e as i64);
                }
            };
            if parent == cur {
                // 到达文件系统根但不是 chroot 根（被卸载/孤儿目录）：
                // 按原版 prepend_path 的语义报「不可达」。
                if cur != pwd { crate::fs::inode::iput(cur); }
                return -(ENOENT as i64);
            }
            let mut name = [0u8; 255];
            let nlen = crate::fs::namei::lookup_ino_name(parent, cur_ino, &mut name);
            // 换到父目录：还掉子目录的额外引用（pwd 是任务持有的，不还）
            if cur != pwd { crate::fs::inode::iput(cur); }
            let nlen = match nlen {
                Some(n) if depth < 32 => n,
                _ => {
                    if parent != root { crate::fs::inode::iput(parent); }
                    return if depth >= 32 { -(ENAMETOOLONG as i64) } else { -(ENOENT as i64) };
                }
            };
            comps[depth] = (name, nlen);
            depth += 1;
            cur = parent;
        }
        // 末尾 cur==root 是 lookup_one 多拿的引用（0 次迭代时例外：pwd==root）
        if depth > 0 { crate::fs::inode::iput(cur); }

        // 组装 "/a/b/c"
        let mut path_len = 1usize; // 开头的 '/'
        for d in 0..depth {
            path_len += 1 + comps[d].1;
        }
        if size < path_len + 1 {
            return -(ERANGE as i64);
        }
        let mut w = buf;
        if depth == 0 {
            *w = b'/';
            w = w.add(1);
        } else {
            for d in (0..depth).rev() {
                *w = b'/';
                w = w.add(1);
                core::ptr::copy_nonoverlapping(comps[d].0.as_ptr(), w, comps[d].1);
                w = w.add(comps[d].1);
            }
        }
        *w = 0;
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
        let r = crate::fs::namei::do_unlink(old_path);
        if r == 0 {
            notify_path_event(old_path, crate::fs::event::IN_MOVED_FROM);
            notify_path_event(new_path, crate::fs::event::IN_MOVED_TO);
        }
        r
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
    let r = unsafe { crate::fs::namei::do_unlink(path) };
    if r == 0 {
        notify_path_event(path, crate::fs::event::IN_DELETE);
    }
    r
}

/// 创建目录。对应原版 `fs/namei.c:sys_mkdir()`。
pub fn mkdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    let r = unsafe { crate::fs::namei::do_mkdir(path, args.a1 as u16) };
    if r == 0 {
        notify_path_event(path, crate::fs::event::IN_CREATE | crate::fs::event::IN_ISDIR);
    }
    r
}

/// 删除目录。对应原版 `fs/namei.c:sys_rmdir()`。
pub fn rmdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    let r = unsafe { crate::fs::namei::do_rmdir(path) };
    if r == 0 {
        notify_path_event(path, crate::fs::event::IN_DELETE | crate::fs::event::IN_ISDIR);
    }
    r
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
    // 真实符号链接（S_IFLNK inode）：minix 目标存第一数据块，ext2/ext4
    // ≤60 字节走快链接（内联 i_block）。不再是「普通文件存目标路径」的近似。
    // SAFETY: 系统调用上下文。
    unsafe { crate::fs::namei::do_symlink(target, link_path) }
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

    // 真实路径：用 lnamei 找到路径末尾的 inode（不跟随末尾链接），再走
    // fs 层的 read_symlink_target。旧实现手工读 ip.data[0]+bread(0x0101)，
    // 对 ext4 快链接（目标内联在 i_block 里）会把目标文本前 4 字节当块号读垃圾，
    // 导致 g++/cc1plus 解析相对符号链接时死循环 readlink。
    let inr = match unsafe { crate::fs::namei::lnamei(path) } {
        Ok(i) => i, Err(e) => return e as i64,
    };
    // 末尾不是符号链接时返回 -EINVAL（POSIX 语义）。glibc realpath / GCC 文件
    // 搜索靠 EINVAL 区分「不是链接」和「不存在」；返回 ENOENT 会让它们对普通
    // 目录（如 /usr）死循环 readlink（g++/cc1plus 卡住的根因）。
    let is_lnk = unsafe { crate::fs::mode::is_lnk(crate::fs::inode::inode(inr).i_mode) };
    if !is_lnk {
        unsafe { crate::fs::inode::iput(inr); }
        return -(EINVAL as i64);
    }
    let target = unsafe { crate::fs::namei::read_symlink_target(inr) };
    unsafe { crate::fs::inode::iput(inr); }
    match target {
        Some((tgt, n)) => {
            let n = core::cmp::min(n, bufsize);
            unsafe { core::ptr::copy_nonoverlapping(tgt.as_ptr(), buf, n); }
            n as i64
        }
        None => -(ENOENT as i64),
    }
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
        // F_DUPFD (0) / F_DUPFD_CLOEXEC (1030): duplicate fd, return >= arg。
        // 内核不追踪 close-on-exec 位（execve 后 fd 表保留），两者语义一致。
        0 | 1030 => {
            let start = arg.max(0);
            // 管道 fd：复制管道注册（同一端）。
            if crate::fs::pipe::fd_is_pipe(fd as usize) {
                let mut new_fd = start;
                while new_fd < crate::fs::NR_OPEN {
                    if !crate::fs::pipe::fd_is_pipe(new_fd)
                        && !crate::net::socket::fd_is_socket(new_fd)
                        && unsafe { crate::fs::open::task_fd(crate::sched::current_index(), new_fd) == crate::fs::inode::NIL }
                    {
                        crate::fs::pipe::dup_fd(fd as usize, new_fd);
                        return new_fd as i64;
                    }
                    new_fd += 1;
                }
                return -(crate::klib::errno::EMFILE as i64);
            }
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
        // F_GETFD：返回该 fd 的 close-on-exec 标志（FD_CLOEXEC=1）
        1 => {
            if fd < 0 || fd as usize >= crate::fs::NR_OPEN { return -(EBADF as i64); }
            let nr = sched::current_index();
            let coe = unsafe { (*sched::task_ptr(nr)).close_on_exec };
            ((coe >> (fd as usize & 63)) & 1) as i64
        }
        // F_SETFD：按 arg 的 bit0 设/清 close-on-exec
        2 => {
            if fd < 0 || fd as usize >= crate::fs::NR_OPEN { return -(EBADF as i64); }
            let nr = sched::current_index();
            let t = unsafe { sched::task_ptr(nr) };
            unsafe {
                if arg & 1 != 0 {
                    (*t).close_on_exec |= 1u64 << (fd as usize & 63);
                } else {
                    (*t).close_on_exec &= !(1u64 << (fd as usize & 63));
                }
            }
            0
        }
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
    // 作业控制 ioctl（tcsetpgrp / tcgetpgrp / 控制终端）。
    if cmd == 0x540E { // TIOCSCTTY
        return unsafe { crate::drivers::char_dev::tty::tty_ioctl_sctty(fd as usize, arg) };
    }
    if cmd == 0x540F { // TIOCGPGRP
        return unsafe { crate::drivers::char_dev::tty::tty_ioctl_gpgrp(fd as usize, arg) };
    }
    if cmd == 0x5410 { // TIOCSPGRP
        return unsafe { crate::drivers::char_dev::tty::tty_ioctl_spgrp(fd as usize, arg) };
    }
    -(EINVAL as i64)
}

/// 访问权限检查。对应原版 `fs/open.c:sys_access()`。
pub fn access(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let mode = args.a1 as u16;
    if pathname.is_null() { return -(EFAULT as i64); }
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 进程上下文；access 用真实 uid/gid（use_effective=false）。
    unsafe { crate::fs::open::sys_access(path, mode, false) }
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

    // Register pipe fds (read end / write end)
    pipe::register_fd(fd_r as usize, pipe_idx, pipe::PIPE_DIR_READ);
    pipe::register_fd(fd_w as usize, pipe_idx, pipe::PIPE_DIR_WRITE);

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

/// 改变所有者（跟随符号链接）。对应原版 `fs/open.c:sys_chown()`。
pub fn chown(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let owner = args.a1 as u32;
    let group = args.a2 as u32;

    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 进程上下文；路径已从用户态拷入。
    unsafe { crate::fs::open::sys_chown(path, owner, group, true) }
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
    let mut denied = 0i32;
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
                if crate::signal::send_sig(sig as u32, i, 0) == 0 {
                    sent += 1;
                } else {
                    denied += 1;
                }
            }
        }
    }
    // 全被权限拒 → EPERM；有目标但一个都没投出去且不是权限问题 → ESRCH
    if sent > 0 { 0 }
    else if denied > 0 { -(crate::klib::errno::EPERM as i64) }
    else { -(crate::klib::errno::ESRCH as i64) }
}

/// 设置 alarm。对应原版 `kernel/sched.c:sys_alarm()`。
pub fn alarm(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let seconds = args.a0 as u64;
    // 等价于 setitimer(ITIMER_REAL, {seconds,0}, NULL)，返回旧的剩余秒数
    let nr = sched::current_index();
    let t = unsafe { sched::task_ptr(nr) };
    let hz = sched::task::HZ;
    let old = unsafe { (*t).it_real_value };
    unsafe {
        (*t).it_real_value = seconds * hz;
        (*t).it_real_incr = 0;
    }
    (old / hz) as i64
}

/// 获取当前时间。对应原版 `kernel/time.c:sys_gettimeofday()`。
/// 秒数 = startup_time（RTC）+ jiffies/HZ；微秒取当前秒的 tick 尾数。
pub fn gettimeofday(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let tv = args.a0 as *mut TimeVal;
    let tz = args.a1 as *mut Timezone;
    unsafe {
        if !tv.is_null() {
            let hz = sched::task::HZ;
            let j = sched::jiffies();
            (*tv).tv_sec = sched::current_time() as i64;
            (*tv).tv_usec = ((j % hz) * 1_000_000 / hz) as i64;
        }
        if !tz.is_null() {
            (*tz).tz_minuteswest = 0;
            (*tz).tz_dsttime = 0;
        }
    }
    0
}

/// 是否超级用户。对应原版 `suser()`：有效 uid == 0。
///
/// # Safety
/// 进程上下文（读 `current()`）。
#[inline]
unsafe fn suser() -> bool {
    // SAFETY: 契约转交。
    unsafe { crate::sched::current().euid == 0 }
}

/// 获取用户 ID。对应原版 `kernel/sys.c:sys_getuid()`。
pub fn getuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 进程上下文，单核。
    unsafe { crate::sched::current().uid as i64 }
}
/// 获取有效用户 ID。对应原版 `kernel/sys.c:sys_geteuid()`。
pub fn geteuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 进程上下文，单核。
    unsafe { crate::sched::current().euid as i64 }
}
/// 获取组 ID。对应原版 `kernel/sys.c:sys_getgid()`。
pub fn getgid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 进程上下文，单核。
    unsafe { crate::sched::current().gid as i64 }
}
/// 获取有效组 ID。对应原版 `kernel/sys.c:sys_getegid()`。
pub fn getegid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 进程上下文，单核。
    unsafe { crate::sched::current().egid as i64 }
}
/// 设置用户 ID。对应原版 `kernel/sys.c:sys_setuid()`。
pub fn setuid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let uid = args.a0 as u32;
    // SAFETY: 进程上下文，单核。
    unsafe {
        let c = crate::sched::current();
        if c.euid == 0 {
            c.uid = uid; c.euid = uid; c.suid = uid; c.fsuid = uid;
        } else if uid == c.uid || uid == c.suid {
            c.euid = uid; c.fsuid = uid;
        } else {
            return -(EPERM as i64);
        }
    }
    0
}
/// 设置组 ID。对应原版 `kernel/sys.c:sys_setgid()`。
pub fn setgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let gid = args.a0 as u32;
    // SAFETY: 进程上下文，单核。
    unsafe {
        let c = crate::sched::current();
        if c.euid == 0 {
            c.gid = gid; c.egid = gid; c.sgid = gid; c.fsgid = gid;
        } else if gid == c.gid || gid == c.sgid {
            c.egid = gid; c.fsgid = gid;
        } else {
            return -(EPERM as i64);
        }
    }
    0
}
/// 设置进程组。对应原版 `kernel/sys.c:sys_setpgid()`。
///
/// `pid == 0` 表示当前进程；`pgid == 0` 表示「以目标进程 pid 建立新组」。
/// 校验（对齐原版）：
///   - 目标进程存在，否则 -ESRCH；
///   - 目标进程与调用者在同一会话（或就是调用者本身），否则 -EPERM；
///   - 目标进程不是会话首进程（会话首进程不能改进程组），否则 -EPERM；
///   - `pgid` 要么等于目标 pid（建新组），要么是本会话里已存在的进程组。
pub fn setpgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pid = args.a0 as i32;
    let pgid = args.a1 as i32;

    // SAFETY: 只读任务表；系统调用上下文，单核。
    unsafe {
        let cur_idx = sched::current_index();
        let cur_session = (*sched::task_ptr(cur_idx)).session;
        let cur_pid = (*sched::task_ptr(cur_idx)).pid;

        // 解析目标 pid。
        let target_pid = if pid == 0 { cur_pid } else { pid };
        let mut target_idx = usize::MAX;
        for i in 0..sched::NR_TASKS {
            let t = sched::task_ptr(i);
            if (*t).state == crate::sched::task::TaskState::Unused { continue; }
            if (*t).pid == target_pid { target_idx = i; break; }
        }
        if target_idx == usize::MAX {
            return -(crate::klib::errno::ESRCH as i64);
        }

        let target_session = (*sched::task_ptr(target_idx)).session;
        let target_own_pid = (*sched::task_ptr(target_idx)).pid;
        // 会话检查：目标必须是调用者自身，或与调用者同会话。
        if target_idx != cur_idx && target_session != cur_session {
            return -(crate::klib::errno::EPERM as i64);
        }
        // 会话首进程不能改进程组。
        if target_session == target_own_pid {
            return -(crate::klib::errno::EPERM as i64);
        }

        // 解析目标 pgid。
        let new_pgid = if pgid == 0 { target_pid } else { pgid };
        // 校验：等于目标 pid（建新组），或是本会话中已存在的进程组。
        let mut valid = new_pgid == target_pid;
        if !valid {
            for i in 0..sched::NR_TASKS {
                let t = sched::task_ptr(i);
                if (*t).state == crate::sched::task::TaskState::Unused { continue; }
                if (*t).session == target_session && (*t).pgrp == new_pgid {
                    valid = true;
                    break;
                }
            }
        }
        if !valid {
            return -(crate::klib::errno::EPERM as i64);
        }

        // 写回。
        (*sched::task_ptr(target_idx)).pgrp = new_pgid;
        0
    }
}

/// 创建会话。对应原版 `kernel/sys.c:sys_setsid()`。
///
/// 调用者不能是进程组首进程（否则 -EPERM）；成功后成为新会话首进程和
/// 新进程组首进程（session == pgrp == pid），并脱离控制终端。
pub fn setsid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 只读/写当前任务；系统调用上下文。
    unsafe {
        let idx = sched::current_index();
        let pid = (*sched::task_ptr(idx)).pid;
        let pgrp = (*sched::task_ptr(idx)).pgrp;
        if pgrp == pid {
            return -(crate::klib::errno::EPERM as i64);
        }
        (*sched::task_ptr(idx)).session = pid;
        (*sched::task_ptr(idx)).pgrp = pid;
        0
    }
}

/// 同步文件系统。对应原版 `sys_sync()`。
///
/// 之前是返回 0 的存根：`sync` 命令「成功」但什么都不刷。结果 chmod/ln 等
/// 只改 inode 元数据（i_mode/i_nlink）的操作，其脏 inode 缓冲从不落盘，
/// 硬关机后权限/链接数回退，e2fsck 报 "ref count wrong"。dev=0 表示所有设备
/// （`sync_buffers`/`sync_supers`/`sync_inodes` 都对 0 做「全部」分支）。
pub fn sync(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 系统调用上下文（进程上下文，可睡）。
    unsafe { crate::fs::buffer::sync_dev(0) };
    0
}
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
pub fn ftruncate(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: fs 层校验 fd。
    unsafe { crate::fs::open::sys_ftruncate(fd as usize, args.a1 as u32) }
}
/// 获取目录项。
pub fn getdents(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // getdents 是批量接口：循环填充直到缓冲放不下下一项或目录读完。
    // 每项是变长的 linux_dirent64 记录：d_ino(8) d_off(8) d_reclen(2)
    // d_type(1) d_name(NUL 结尾)，reclen 按 8 对齐。原版 1.0.9 的
    // sys_readdir 一次一项，但 getdents(78)/getdents64(217) 是
    // 现代批量语义，glibc 的 readdir 一次 read 一整个目录靠的就是它。
    let need = core::mem::size_of::<crate::fs::Dirent>() as u64;
    if args.a2 < need {
        return -(EINVAL as i64);
    }
    // 用 user_ok（低 1GB 直过 + 高位查页表）而非 check_range 1GB：
    // readdir 的缓冲可能是高位 malloc/mmap 出来的，1GB 护栏会误拒。
    if !unsafe { user_ok(args.a1, args.a2, crate::mm::area::AccessMode::Write) } {
        return -(EFAULT as i64);
    }
    let mut written = 0u64;
    loop {
        if args.a2 - written < need {
            break;
        }
        // 用 zeroed() 而不是结构体字面量：Dirent 带 #[repr(C)]，d_name 之后的
        // 尾随 padding（对齐到 8）字面量不初始化，会把内核栈垃圾写进用户缓冲。
        let mut d: crate::fs::Dirent = unsafe { core::mem::zeroed() };
        d.d_reclen = need as u16;
        // SAFETY: 系统调用上下文，fs 层会睡；fd 无效返回 -EBADF。
        let r = unsafe { crate::fs::read_write::readdir(fd as usize, &mut d) };
        if r <= 0 {
            if written > 0 {
                break; // 已写出若干项：EOF/错误留给下一次调用
            }
            return r;
        }
        // SAFETY: user_ok 已确认 [a1, a1+a2) 可写，written+need<=a2。
        unsafe { ((args.a1 + written) as *mut crate::fs::Dirent).write_unaligned(d) };
        written += need;
    }
    written as i64
}
/// 获取目录项64。
pub fn getdents64(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    // 本树的 Dirent 已经是 64 位字段（d_ino: u64），两者布局一致。
    getdents(args, regs)
}
/// 文件描述符控制。
pub fn fchdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: fs 层校验 fd。
    unsafe { crate::fs::open::sys_fchdir(fd as usize) }
}
/// 设置/获取 umask。对应原版 `kernel/sys.c:sys_umask()`：
/// 设置新值（只取 `S_IRWXUGO` 位）并返回旧值。
pub fn umask(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let mask = args.a0 as u16;
    // SAFETY: 进程上下文，单核。
    unsafe {
        let c = crate::sched::current();
        let old = c.umask;
        c.umask = mask & 0o777;
        old as i64
    }
}
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

            // Pipe fds：真实就绪状态（有数据/EOF 可读，未满可写，
            // 对端全关报 POLLHUP）。
            if crate::fs::pipe::fd_is_pipe(fd) {
                let (rd, wr, hup) = crate::fs::pipe::fd_poll_status(fd);
                if rd && pfd.events & 1 != 0 { pfd.revents |= 1; }  // POLLIN
                if wr && pfd.events & 4 != 0 { pfd.revents |= 4; }  // POLLOUT
                if hup { pfd.revents |= 0x10; }                     // POLLHUP
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

    // 先快照输入掩码（输出要原地清空，直接改会把「用户关心哪些 fd」弄丢；
    // 旧实现不管用户有没有把 fd 放进 readfds/writefds 都两边置位）。
    let mut rin = [0u64; 16];
    let mut win = [0u64; 16];
    unsafe {
        for w in 0..nwords {
            if !readfds.is_null() { rin[w] = core::ptr::read_volatile(readfds.add(w)); }
            if !writefds.is_null() { win[w] = core::ptr::read_volatile(writefds.add(w)); }
        }
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

    let mut total_ready = 0i64;
    for fd in 0..nfds {
        let word = fd / 64;
        let bit = fd % 64;
        let want_r = rin[word] & (1u64 << bit) != 0;
        let want_w = win[word] & (1u64 << bit) != 0;
        if !want_r && !want_w { continue; }

        let (mut rd, mut wr) = (false, false);
        if crate::fs::pipe::fd_is_pipe(fd) {
            // 管道真实就绪：读端有数据/EOF 可读；写端未满可写。
            let (r, w, hup) = crate::fs::pipe::fd_poll_status(fd);
            rd = r || hup;
            wr = w || hup;
        } else {
            // 普通文件/字符设备总是就绪（同原版 file_select 的默认返回）。
            let filp = unsafe { crate::fs::open::fd_to_filp(fd) };
            if filp != crate::fs::inode::NIL { rd = true; wr = true; }
        }
        unsafe {
            if rd && want_r {
                core::ptr::write_volatile(readfds.add(word),
                    core::ptr::read_volatile(readfds.add(word)) | (1u64 << bit));
                total_ready += 1;
            }
            if wr && want_w {
                core::ptr::write_volatile(writefds.add(word),
                    core::ptr::read_volatile(writefds.add(word)) | (1u64 << bit));
                total_ready += 1;
            }
        }
    }
    total_ready
}
/// 挂载文件系统。
pub fn mount(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    unsafe {
        let fstype_ptr = args.a2 as *const u8;
        let target_ptr = args.a1 as *const u8;
        if fstype_ptr.is_null() || target_ptr.is_null() { return -(EINVAL as i64); }
        let mut fstype_buf = [0u8; 16];
        for i in 0..15 { let b = core::ptr::read_volatile(fstype_ptr.add(i)); if b == 0 { break; } fstype_buf[i] = b; }
        let fl = fstype_buf.iter().position(|&b| b == 0).unwrap_or(15);
        let fstype = &fstype_buf[..fl];
        let mut target_buf = [0u8; 128];
        for i in 0..127 { let b = core::ptr::read_volatile(target_ptr.add(i)); if b == 0 { break; } target_buf[i] = b; }
        let tl = target_buf.iter().position(|&b| b == 0).unwrap_or(127);
        let target = &target_buf[..tl];
        let dir_inode = match crate::fs::namei::namei(target) { Ok(n) => n, Err(_) => return -(ENOENT as i64) };
        match fstype {
            b"proc" => crate::fs::proc::mount_proc(dir_inode),
            b"tmpfs" => crate::fs::tmpfs::mount_tmpfs(dir_inode),
            _ => -(ENODEV as i64),
        }
    }
}
/// 卸载文件系统。对应原版 `fs/super.c:sys_umount()`。
/// 挂载点 inode 的 `i_mount` 清回 NIL（被盖目录重新可见），并 iput
/// 被挂文件系统的根 inode。根文件系统（i_mount==NIL 的 "/"）与
/// 非挂载点返回 -EINVAL。
pub fn umount(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    let dir = match unsafe { crate::fs::namei::namei(path) } {
        Ok(n) => n,
        Err(_) => return -(ENOENT as i64),
    };
    // SAFETY: 进程上下文；inode 表访问单核串行。
    unsafe {
        let ip = crate::fs::inode::inode_ptr(dir);
        let mounted = (*ip).i_mount;
        if mounted == crate::fs::inode::NIL {
            return -(EINVAL as i64);
        }
        (*ip).i_mount = crate::fs::inode::NIL;
        crate::fs::inode::iput(mounted);
    }
    0
}
/// 设置/获取资源限制（`prlimit64`）。pid=0 操作当前进程，否则按 pid 查
/// 任务表；权限规则同 [`setrlimit`]。
pub fn prlimit64(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // prlimit64(pid, resource, new_limit, old_limit)
    let pid = args.a0 as i32;
    let resource = args.a1 as usize;
    let new_limit = args.a2;
    let old_limit = args.a3;
    if resource >= sched::task::RLIM_NLIMITS {
        return -(EINVAL as i64);
    }
    // pid==0 是自己；否则按 pid 找任务
    let nr = if pid <= 0 {
        sched::current_index()
    } else {
        let mut found = usize::MAX;
        for i in 0..sched::NR_TASKS {
            let t = unsafe { sched::task_ptr(i) };
            if unsafe { (*t).state } != sched::task::TaskState::Unused
                && unsafe { (*t).pid } == pid
            {
                found = i;
                break;
            }
        }
        if found == usize::MAX {
            return -(ESRCH as i64);
        }
        found
    };
    let t = unsafe { sched::task_ptr(nr) };
    if old_limit != 0 {
        let r = unsafe { (*t).rlim[resource] };
        let rlim = old_limit as *mut RLimit;
        // SAFETY: 用户指针，rlimit 是 POD。
        unsafe { (*rlim).rlim_cur = r.rlim_cur; (*rlim).rlim_max = r.rlim_max; }
    }
    if new_limit != 0 {
        // SAFETY: 用户指针，rlimit 是 POD。
        let new = unsafe { (*(new_limit as *const RLimit)) };
        let old = unsafe { (*t).rlim[resource] };
        if new.rlim_cur > new.rlim_max {
            return -(EINVAL as i64);
        }
        if new.rlim_max > old.rlim_max || new.rlim_cur > old.rlim_max {
            if unsafe { (*t).euid } != 0 {
                return -(EPERM as i64);
            }
        }
        unsafe { (*t).rlim[resource] = sched::task::Rlimit { rlim_cur: new.rlim_cur, rlim_max: new.rlim_max }; }
    }
    0
}
/// 重新引导。对应原版 `kernel/sys.c:sys_reboot()`。
/// 魔法数校验后按 cmd 走：RESTART 用键盘控制器复位线，HALT/POWER_OFF 停机。
pub fn reboot(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    const LINUX_REBOOT_MAGIC1: i32 = 0xfee1dead_u32 as i32;
    const LINUX_REBOOT_MAGIC2: i32 = 672274793; // Linus 的生日
    const LINUX_REBOOT_MAGIC2B: i32 = 85072278;
    const LINUX_REBOOT_MAGIC2C: i32 = 369367448;
    const LINUX_REBOOT_MAGIC2D: i32 = 537993216;
    const LINUX_REBOOT_CMD_RESTART: u32 = 0x01234567;
    const LINUX_REBOOT_CMD_HALT: u32 = 0xCDEF0123;
    const LINUX_REBOOT_CMD_POWER_OFF: u32 = 0x4321FEDC;

    let magic1 = args.a0 as i32;
    let magic2 = args.a1 as i32;
    let cmd = args.a2 as u32;
    if magic1 != LINUX_REBOOT_MAGIC1
        || (magic2 != LINUX_REBOOT_MAGIC2 && magic2 != LINUX_REBOOT_MAGIC2B
            && magic2 != LINUX_REBOOT_MAGIC2C && magic2 != LINUX_REBOOT_MAGIC2D)
    {
        return -(EINVAL as i64);
    }
    match cmd {
        LINUX_REBOOT_CMD_RESTART => {
            crate::sprintln!("reboot: restarting");
            // 键盘控制器脉冲复位线（8042 的 pulse output 0xFE）
            // SAFETY: CPL=0，写 8042 命令端口是标准复位序列
            unsafe {
                core::arch::asm!("outb %al, %dx", in("dx") 0x64u16, in("al") 0xFEu8,
                                 options(nomem, nostack, preserves_flags, att_syntax));
            }
            // 8042 没复位（无控制器）→ 三重故障兜底
            // SAFETY: lidt 一个空 IDT 后 int3，必三重故障
            unsafe {
                let zero: [u8; 10] = [0; 10];
                core::arch::asm!("lidt ({0}); int3", in(reg) &zero,
                                 options(nostack, att_syntax));
            }
            loop { core::hint::spin_loop(); }
        }
        LINUX_REBOOT_CMD_HALT | LINUX_REBOOT_CMD_POWER_OFF => {
            crate::sprintln!("reboot: halt/poweroff");
            // SAFETY: 关机路径，关中断停机
            unsafe {
                core::arch::asm!("cli; hlt", options(nomem, nostack));
            }
            loop { core::hint::spin_loop(); }
        }
        _ => -(EINVAL as i64),
    }
}
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
    use crate::sched::task::{STACK_MAGIC, TaskState};
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

            // 先把栈拿到手，失败了就不用回滚任务表。内核栈从池里取
            // （连续、对齐），不能用单页 get_free_page（KSTACK_SIZE=4 页）。
            let stack_page = sched::alloc_kstack();
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
            // Copy page tables: child gets COW copy of parent
            if (*parent).pml4 != 0 {
                let child_pml4 = crate::mm::paging::alloc_pml4();
                if child_pml4 == 0 || !crate::mm::paging::clone_kernel_pdpt(child_pml4)
                    || !crate::mm::paging::cow_copy_page_table((*parent).pml4, child_pml4)
                {
                    if child_pml4 != 0 { crate::mm::free_page(child_pml4); }
                    sched::free_kstack(stack_page);
                    return -(EAGAIN as i64);
                }
                (*child).pml4 = child_pml4;
                (*child).tss.cr3 = child_pml4 as u64;
                // file-backed mmap 的 VMA 记账也要随地址空间克隆，
                // 否则子进程对 reserved 文件页的缺页会落成匿名零页。
                crate::mm::mmap_vma::clone_table(parent_nr, child_nr);
            }
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

            // 复制 fd 表（旁路数组 TASK_FILP，不在 Task 里，clone 不会带过去）。
            // 每个被共享的打开文件表项 f_count++（原版 copy_process 里
            // `for (i=0; i<NR_OPEN; i++) if (f=p->filp[i]) f->f_count++;`）。
            // 不做这步的话子进程没有 stdin/stdout/stderr，cat 这类外部命令
            // 一 openat 就拿到 fd 0、write(1) 直接 EBADF。
            crate::fs::open::clone_fds(parent_nr, child_nr);
            // 管道 fd 表也要复制（否则子进程里的管道 fd 丢失/串台）。
            crate::fs::pipe::clone_pipe_fds(parent_nr, child_nr);
            for fd in 0..crate::fs::NR_OPEN {
                let fi = crate::fs::open::task_fd(child_nr, fd);
                if fi != crate::fs::inode::NIL {
                    // SAFETY: fi 是有效的 file_table 下标。
                    (*crate::fs::file_table::filp(fi)).f_count += 1;
                }
            }

            // ---- 布置子进程内核栈 ----
            // 布局和 sched::kernel_thread 一致，只是 ret 地址上方放的不是
            // fn/arg 而是一整份 pt_regs：
            //   [tss.rsp + 0 .. +48]  switch_to 的 7 个保存槽
            //   [tss.rsp + 56]        返回地址 = ret_from_fork
            //   [tss.rsp + 64 ..]     pt_regs（ret_from_sys_call 要用）
            let stack_top = stack_page as u64 + crate::sched::KSTACK_SIZE as u64;
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
/// 设置间隔定时器。对应原版 `kernel/itimer.c:sys_setitimer()`。
/// 三种定时器都在 `do_timer` 里递减并投递信号（REAL→SIGALRM、
/// VIRTUAL→SIGVTALRM、PROF→SIGPROF），到期用 interval 重装。
pub fn setitimer(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let which = args.a0 as i32;
    let new = args.a1 as *const ItimerVal;
    let old = args.a2 as *mut ItimerVal;
    if which < 0 || which > 2 {
        return -(EINVAL as i64);
    }
    let nr = sched::current_index();
    let t = unsafe { sched::task_ptr(nr) };
    if !old.is_null() {
        // SAFETY: 用户指针，itimer 字段是 POD。
        unsafe { write_itimer(old, t, which as usize) };
    }
    if new.is_null() {
        return 0;
    }
    // SAFETY: 用户指针，itimer 字段是 POD。
    let (val, inter) = unsafe { ((*new).it_value, (*new).it_interval) };
    if val.tv_sec < 0 || val.tv_usec < 0 || val.tv_usec >= 1_000_000
        || inter.tv_sec < 0 || inter.tv_usec < 0 || inter.tv_usec >= 1_000_000
    {
        return -(EINVAL as i64);
    }
    // timeval → tick：非零至少 1 tick（原版 1.0.9 是「sec*HZ + usec*HZ/1000000」，下取整）
    let to_ticks = |tv: TimeVal| -> u64 {
        if tv.tv_sec == 0 && tv.tv_usec == 0 { return 0; }
        let hz = sched::task::HZ;
        (tv.tv_sec as u64 * hz + (tv.tv_usec as u64 * hz) / 1_000_000).max(1)
    };
    unsafe {
        let (v, i) = match which {
            0 => (&mut (*t).it_real_value, &mut (*t).it_real_incr),
            1 => (&mut (*t).it_virt_value, &mut (*t).it_virt_incr),
            _ => (&mut (*t).it_prof_value, &mut (*t).it_prof_incr),
        };
        *v = to_ticks(val);
        *i = to_ticks(inter);
    }
    0
}

/// 把任务 `t` 的第 `which` 个 itimer 读成 itimerval 写到用户 `out`。
///
/// # Safety
/// `out` 必须指向可写的用户 itimerval。
unsafe fn write_itimer(out: *mut ItimerVal, t: *const sched::task::Task, which: usize) {
    let (v, i) = unsafe {
        match which {
            0 => ((*t).it_real_value, (*t).it_real_incr),
            1 => ((*t).it_virt_value, (*t).it_virt_incr),
            _ => ((*t).it_prof_value, (*t).it_prof_incr),
        }
    };
    let hz = sched::task::HZ;
    let to_tv = |ticks: u64| TimeVal {
        tv_sec: (ticks / hz) as i64,
        tv_usec: ((ticks % hz) * 1_000_000 / hz) as i64,
    };
    // SAFETY: 契约转交。
    unsafe {
        (*out).it_value = to_tv(v);
        (*out).it_interval = to_tv(i);
    }
}
/// 取间隔定时器。对应原版 `kernel/itimer.c:sys_getitimer()`。
pub fn getitimer(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let which = args.a0 as i32;
    let v = args.a1 as *mut ItimerVal;
    if v.is_null() {
        return -(EFAULT as i64);
    }
    if which < 0 || which > 2 {
        return -(EINVAL as i64);
    }
    let nr = sched::current_index();
    let t = unsafe { sched::task_ptr(nr) };
    // SAFETY: 用户指针，itimer 字段是 POD。
    unsafe { write_itimer(v, t, which as usize) };
    0
}

// Memory syscalls
pub fn mlock(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn munlock(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn mlockall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn munlockall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 重映射/搬移一段虚拟内存。对应现代内核 `mm/mremap.c`（1.0.9 无）。
/// 支持 MREMAP_MAYMOVE（找不到原地空间就整体搬走）与 MREMAP_FIXED。
/// 物理页不拷贝——只搬叶子页表项（PRESENT 页连物理页一起过户，
/// RESERVED/SWAPPED 叶子原样搬），glibc 大 arena 的 realloc 靠它。
pub fn mremap(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::{EINVAL, ENOMEM};
    use crate::mm::{page, paging};

    const MREMAP_MAYMOVE: u64 = 1;
    const MREMAP_FIXED: u64 = 2;

    let old_addr = args.a0 as usize;
    let old_len = args.a1 as usize;
    let new_len = args.a2 as usize;
    let mflags = args.a3;
    let new_addr_arg = args.a4 as usize;

    if old_addr & (page::PAGE_SIZE - 1) != 0 || new_len == 0 {
        return -(EINVAL as i64);
    }
    if mflags & MREMAP_FIXED != 0 && mflags & MREMAP_MAYMOVE == 0 {
        return -(EINVAL as i64);
    }
    let old_npages = (old_len + page::PAGE_SIZE - 1) / page::PAGE_SIZE;
    let new_npages = (new_len + page::PAGE_SIZE - 1) / page::PAGE_SIZE;

    unsafe {
        let nr = sched::current_index();
        let t = sched::task_ptr(nr);
        let pml4 = (*t).pml4;
        let pml4 = if pml4 == 0 { 0x4000usize } else { pml4 };

        // 缩小：原地截断，尾部按 munmap 处理。
        if new_npages <= old_npages {
            let tail = old_addr + new_npages * page::PAGE_SIZE;
            for i in new_npages..old_npages {
                let va = old_addr + i * page::PAGE_SIZE;
                paging::unmap_page(pml4, va);
            }
            crate::mm::mmap_vma::remove_range(
                nr, tail, old_addr + old_npages * page::PAGE_SIZE,
            );
            return old_addr as i64;
        }

        // 扩大：简化实现——不在原地探测增长空间，有 MAYMOVE 就整体搬到
        // mmap_base 向下新分的区间（或 MREMAP_FIXED 指定的地址）。
        if mflags & MREMAP_MAYMOVE == 0 {
            return -(ENOMEM as i64);
        }
        let span = new_npages * page::PAGE_SIZE;
        let new_addr = if mflags & MREMAP_FIXED != 0 {
            if new_addr_arg & (page::PAGE_SIZE - 1) != 0 {
                return -(EINVAL as i64);
            }
            crate::mm::mmap_vma::remove_range(nr, new_addr_arg, new_addr_arg + span);
            new_addr_arg
        } else {
            let base = (*t).mmap_base;
            if base < span as u64 {
                return -(ENOMEM as i64);
            }
            let alloc_end = ((base - span as u64) as usize) & !0xFFF;
            if alloc_end == 0 {
                return -(ENOMEM as i64);
            }
            (*t).mmap_base = alloc_end as u64;
            alloc_end
        };

        // 逐页搬叶子项：新位置写入旧叶子的原值（含物理页/槽号/标志），
        // 旧位置清零。页引用计数不变——所有权随叶子一起过户。
        for i in 0..old_npages {
            let old_va = old_addr + i * page::PAGE_SIZE;
            let new_va = new_addr + i * page::PAGE_SIZE;
            if let Some(leaf) = paging::leaf_entry(pml4, old_va) {
                if !paging::set_leaf_entry(pml4, new_va, leaf) {
                    return -(ENOMEM as i64);
                }
                if !paging::set_leaf_entry(pml4, old_va, 0) {
                    return -(ENOMEM as i64);
                }
            }
        }

        // VMA 记账跟着搬：摘旧区间，按偏移平移后重挂。
        let mut vma_buf = [crate::mm::mmap_vma::EMPTY; crate::mm::mmap_vma::MAX_VMAS];
        let nvmas = crate::mm::mmap_vma::drain_range(
            nr, old_addr, old_addr + old_npages * page::PAGE_SIZE, &mut vma_buf,
        );
        for v in vma_buf[..nvmas].iter() {
            let shift = new_addr as isize - old_addr as isize;
            let mut nv = *v;
            nv.start = (nv.start as isize + shift) as usize;
            nv.end = (nv.end as isize + shift) as usize;
            if !crate::mm::mmap_vma::add(nr, nv) {
                crate::pr_warn!("mremap: vma move dropped [{:#x},{:#x})", v.start, v.end);
            }
        }
        new_addr as i64
    }
}
pub fn msync(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

// IPC syscalls
/// SysV 共享内存：获取/创建段。对应原版 `ipc/shm.c:sys_shmget()`。
pub fn shmget(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::mm::shm::sys_shmget(args.a0 as i32, args.a1 as usize, args.a2 as i32)
}
/// SysV 共享内存：附加到当前地址空间。对应原版 `sys_shmat()`。
/// shmaddr==0 时按 mmap_base 向下挑选空闲区（同匿名 mmap）。
pub fn shmat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let id = args.a0 as i64;
    if id < 0 { return -(EINVAL as i64); }
    let shmaddr = args.a1 as usize;
    let nr = sched::current_index();
    let pml4 = unsafe { (*sched::task_ptr(nr)).pml4 };
    if pml4 == 0 { return -(EINVAL as i64); }
    // SAFETY: 系统调用上下文；pick_addr 只改当前任务的 mmap_base。
    unsafe {
        crate::mm::shm::sys_shmat(id as usize, shmaddr, pml4, |nbytes| {
            let t = sched::task_ptr(nr);
            let base = (*t).mmap_base;
            let alloc_end = if base < nbytes as u64 { 0usize } else { (base - nbytes as u64) as usize & !0xFFF };
            if alloc_end != 0 { (*t).mmap_base = alloc_end as u64; }
            alloc_end
        })
    }
}
/// SysV 共享内存：解除附加。对应原版 `sys_shmdt()`。
pub fn shmdt(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let nr = sched::current_index();
    let pml4 = unsafe { (*sched::task_ptr(nr)).pml4 };
    // SAFETY: 系统调用上下文。
    unsafe { crate::mm::shm::sys_shmdt(args.a0 as usize, pml4) }
}
/// SysV 共享内存：控制（IPC_STAT / IPC_RMID）。对应原版 `sys_shmctl()`。
pub fn shmctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let id = args.a0 as i64;
    if id < 0 { return -(EINVAL as i64); }
    // SAFETY: 用户指针由 IPC_STAT 分支写。
    unsafe { crate::mm::shm::sys_shmctl(id as usize, args.a1 as i32, args.a2 as *mut u8) }
}
/// SysV 信号量：获取/创建集合。对应原版 `ipc/sem.c:sys_semget()`。
pub fn semget(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::mm::sem::sys_semget(args.a0 as i32, args.a1 as i32, args.a2 as i32)
}
/// SysV 信号量：一组原子操作。对应原版 `sys_semop()`。
pub fn semop(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let id = args.a0 as i64;
    if id < 0 { return -(EINVAL as i64); }
    crate::mm::sem::sem_op_timed(id as usize, args.a1 as *const u8, args.a2 as usize, None)
}
/// SysV 信号量：控制。对应原版 `sys_semctl()`。
pub fn semctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let id = args.a0 as i64;
    if id < 0 { return -(EINVAL as i64); }
    crate::mm::sem::sys_semctl(id as usize, args.a1 as usize, args.a2 as i32, args.a3)
}
/// SysV 信号量：带超时的操作。对应 Linux 2.6 的 `sys_semtimedop()`。
pub fn semtimedop(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let id = args.a0 as i64;
    if id < 0 { return -(EINVAL as i64); }
    let ts = args.a3 as *const u64;
    let deadline = if ts.is_null() {
        None
    } else {
        // SAFETY: 用户指针，timespec {sec, nsec}。
        let (sec, nsec) = unsafe { (core::ptr::read_volatile(ts), core::ptr::read_volatile(ts.add(1))) };
        if nsec >= 1_000_000_000 { return -(EINVAL as i64); }
        let hz = sched::task::HZ;
        let ticks = sec.saturating_mul(hz).saturating_add((nsec.saturating_mul(hz) + 999_999_999) / 1_000_000_000);
        Some(sched::jiffies().saturating_add(ticks))
    };
    crate::mm::sem::sem_op_timed(id as usize, args.a1 as *const u8, args.a2 as usize, deadline)
}
/// SysV 消息队列：获取/创建队列。对应原版 `ipc/msg.c:sys_msgget()`。
pub fn msgget(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::mm::msg::sys_msgget(args.a0 as i32, args.a1 as i32)
}
/// SysV 消息队列：发送。对应原版 `sys_msgsnd()`。
pub fn msgsnd(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let id = args.a0 as i64;
    if id < 0 { return -(EINVAL as i64); }
    crate::mm::msg::sys_msgsnd(id as usize, args.a1 as *const u8, args.a2 as usize, args.a3 as i32)
}
/// SysV 消息队列：接收。对应原版 `sys_msgrcv()`。
pub fn msgrcv(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let id = args.a0 as i64;
    if id < 0 { return -(EINVAL as i64); }
    crate::mm::msg::sys_msgrcv(
        id as usize, args.a1 as *mut u8, args.a2 as usize,
        args.a3 as i64, args.a4 as i32,
    )
}
/// SysV 消息队列：控制（IPC_RMID / IPC_STAT）。对应原版 `sys_msgctl()`。
pub fn msgctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let id = args.a0 as i64;
    if id < 0 { return -(EINVAL as i64); }
    crate::mm::msg::sys_msgctl(id as usize, args.a1 as i32, args.a2 as *mut u8)
}

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
    // 用 jiffies(100Hz) 充当时间源。接受 realtime/monotonic 及其 coarse/raw/
    // boot 变体（glibc 的 clock_gettime 可能落到 coarse 时钟上），其余（如
    // 进程/线程 CPU 时间）没有实现，返回 EINVAL。
    let supported = matches!(clk_id, 0 | 1 | 4 | 5 | 6 | 7 | 9 | 10 | 11);
    if supported {
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
pub fn clock_nanosleep(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // clock_nanosleep(clockid, flags, req, rem)：req 在 a2、rem 在 a3，
    // 而 nanosleep 期望 req 在 a0、rem 在 a1。之前直接把 a0（clockid，常为
    // CLOCK_REALTIME=0）当 req 指针传下去 → req==NULL → EINVAL，glibc 的
    // nanosleep()（内部走 clock_nanosleep）全部失败，`sleep 1` 报
    // "cannot read realtime clock: Invalid argument"。
    // TIMER_ABSTIME(flags=1) 的绝对时间语义暂不实现，按相对时间睡。
    let na = SysArgs { a0: args.a2, a1: args.a3, a2: 0, a3: 0, a4: 0, a5: 0 };
    nanosleep(&na, _regs)
}

// Priority syscalls
pub fn getpriority(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn setpriority(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

// Hostname syscalls
pub fn sethostname(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let name = args.a0 as *const u8;
    let len = args.a1 as usize;
    if name.is_null() {
        return -(EFAULT as i64);
    }
    if len > 64 {
        return -(EINVAL as i64);
    }
    if !unsafe { user_ok(args.a0, len as u64, crate::mm::area::AccessMode::Read) } {
        return -(EFAULT as i64);
    }
    // SAFETY: user_ok 通过；HOSTNAME 是静态缓冲。
    unsafe {
        let src = core::slice::from_raw_parts(name, len);
        hostname_set(&mut *core::ptr::addr_of_mut!(HOSTNAME), src);
    }
    0
}
pub fn setdomainname(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let name = args.a0 as *const u8;
    let len = args.a1 as usize;
    if name.is_null() {
        return -(EFAULT as i64);
    }
    if len > 64 {
        return -(EINVAL as i64);
    }
    if !unsafe { user_ok(args.a0, len as u64, crate::mm::area::AccessMode::Read) } {
        return -(EFAULT as i64);
    }
    // SAFETY: user_ok 通过；DOMAINNAME 是静态缓冲。
    unsafe {
        let src = core::slice::from_raw_parts(name, len);
        hostname_set(&mut *core::ptr::addr_of_mut!(DOMAINNAME), src);
    }
    0
}

/// getcpu(cpu*, node*, cache*)：报告调用线程所在 CPU。
/// 调度器只在 BSP 上跑（AP 仅响应 run_on_all_cpus 派工），
/// 所以进程视角恒为 cpu 0 / node 0。
pub fn getcpu(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    if args.a0 != 0 {
        // SAFETY: 用户指针；按 u32 写当前 cpu。
        unsafe { core::ptr::write_unaligned(args.a0 as *mut u32, 0) };
    }
    if args.a1 != 0 {
        // SAFETY: 同上，node 恒 0。
        unsafe { core::ptr::write_unaligned(args.a1 as *mut u32, 0) };
    }
    0
}

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
/// 取 `*at` 系列系统调用的路径，并校验 dirfd。
///
/// `AT_FDCWD(-100)` 或绝对路径退化为普通路径解析；相对路径 + 具体 dirfd
/// 目前仍按 CWD 解析（VFS 尚未支持真正的 dirfd 相对解析），但先确认该 fd
/// 有效，避免静默作用到错误文件上。
///
/// # Safety
/// 只能在系统调用上下文调用（`user_path` 依赖 `current()` 与 `check_range`）。
unsafe fn at_path(dirfd: i64, path_ptr: u64) -> Result<&'static [u8], i64> {
    use crate::klib::errno::EBADF;
    // SAFETY: 契约转交 user_path。
    let path = unsafe { user_path(path_ptr) }?;
    if dirfd != -100 && path.first() != Some(&b'/') {
        if crate::fs::open::fd_to_filp(dirfd as usize) == crate::fs::inode::NIL {
            return Err(-(EBADF as i64));
        }
    }
    Ok(path)
}

/// `fchmodat(dirfd, path, mode, flags)`。flags 目前不区分 AT_SYMLINK_NOFOLLOW。
pub fn fchmodat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let path = match unsafe { at_path(args.a0 as i64, args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`chmod`]。
    unsafe { crate::fs::open::sys_chmod(path, args.a2 as u16) }
}

/// `fchownat(dirfd, path, owner, group, flags)`。`AT_SYMLINK_NOFOLLOW` 时不跟随链接。
pub fn fchownat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let path = match unsafe { at_path(args.a0 as i64, args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    let owner = args.a2 as u32;
    let group = args.a3 as u32;
    let flags = args.a4 as u64;
    // AT_SYMLINK_NOFOLLOW = 0x100
    let follow = flags & 0x100 == 0;
    // SAFETY: 进程上下文；路径已从用户态拷入。
    unsafe { crate::fs::open::sys_chown(path, owner, group, follow) }
}
pub fn openat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::EBADF;
    // openat(dirfd, path, flags, mode)。AT_FDCWD 与绝对路径退化为普通 open。
    let dirfd = args.a0 as i32;
    let path = match unsafe { user_path(args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    if dirfd != -100 && path.first() != Some(&b'/') {
        // 相对路径 + 具体 dirfd：目前按 CWD 解析，但先确认 fd 有效，
        // 避免静默打开错的文件。真正的 dirfd 相对解析留待 VFS 完善。
        if crate::fs::open::fd_to_filp(dirfd as usize) == crate::fs::inode::NIL {
            return -(EBADF as i64);
        }
    }
    // O_CLOEXEC 处理同 open()：剥掉标志、成功后置 close_on_exec 位。
    let flags = args.a2 as u32;
    let cloexec = flags & crate::fs::oflags::O_CLOEXEC != 0;
    // SAFETY: 同 sys_open。
    let r = unsafe { crate::fs::open::sys_open(path, flags & !crate::fs::oflags::O_CLOEXEC, args.a3 as u16) };
    if r >= 0 && cloexec {
        let nr = sched::current_index();
        unsafe { (*sched::task_ptr(nr)).close_on_exec |= 1u64 << (r as usize & 63) };
    }
    r
}

/// `mkdirat(dirfd, path, mode)`。glibc 的 `mkdir` 在 x86_64 上走这里。
pub fn mkdirat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let path = match unsafe { at_path(args.a0 as i64, args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`mkdir`]。
    unsafe { crate::fs::namei::do_mkdir(path, args.a2 as u16) }
}

/// `mknodat(dirfd, path, mode, dev)`。
pub fn mknodat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let path = match unsafe { at_path(args.a0 as i64, args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`mknod`]。
    unsafe { crate::fs::namei::do_mknod(path, args.a2 as u16, args.a3 as u16) }
}

/// `unlinkat(dirfd, path, flags)`。`AT_REMOVEDIR`(0x200) 时删目录。
pub fn unlinkat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let path = match unsafe { at_path(args.a0 as i64, args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    let flags = args.a2 as u32;
    const AT_REMOVEDIR: u32 = 0x200;
    // SAFETY: 同 [`unlink`]/[`rmdir`]。
    unsafe {
        if flags & AT_REMOVEDIR != 0 {
            crate::fs::namei::do_rmdir(path)
        } else {
            crate::fs::namei::do_unlink(path)
        }
    }
}

/// `renameat(olddirfd, oldpath, newdirfd, newpath)`。glibc 的 `rename` 走这里。
pub fn renameat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let old = match unsafe { at_path(args.a0 as i64, args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    let new = match unsafe { at_path(args.a2 as i64, args.a3) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`rename`]。
    unsafe {
        let r = crate::fs::namei::do_link(old, new);
        if r < 0 { return r; }
        crate::fs::namei::do_unlink(old)
    }
}

/// `renameat2(..., flags)`。仅支持 flags==0；其余返回 -EINVAL，
/// glibc/coreutils 会回退到 [`renameat`] 或 [`rename`]。
pub fn renameat2(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    if args.a4 != 0 {
        return -(EINVAL as i64);
    }
    renameat(args, _regs)
}

/// `linkat(olddirfd, oldpath, newdirfd, newpath, flags)`。glibc 的 `link` 走这里。
pub fn linkat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let old = match unsafe { at_path(args.a0 as i64, args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    let new = match unsafe { at_path(args.a2 as i64, args.a3) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`link`]。
    unsafe { crate::fs::namei::do_link(old, new) }
}

/// `symlinkat(target, newdirfd, linkpath)`。glibc 的 `symlink` 走这里。
pub fn symlinkat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // 目标文本在 a0、链接路径在 a2；把链接路径搬到 a1 后交给 [`symlink`]。
    let sa = SysArgs { a0: args.a0, a1: args.a2, a2: 0, a3: 0, a4: 0, a5: 0 };
    symlink(&sa, _regs)
}
/// 改文件属主（按 fd）。对应原版 `fs/open.c:sys_fchown()`。
pub fn fchown(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as usize;
    let owner = args.a1 as u32;
    let group = args.a2 as u32;
    // SAFETY: 进程上下文。
    unsafe { crate::fs::open::sys_fchown(fd, owner, group) }
}

pub fn getrusage(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let usage = args.a1 as *mut RUsage;
    if !usage.is_null() { unsafe { (*usage).ru_utime.tv_sec = 0; (*usage).ru_utime.tv_usec = 0; (*usage).ru_stime.tv_sec = 0; (*usage).ru_stime.tv_usec = 0; } }
    0
}
/// 资源限制。
pub fn getrlimit(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let resource = args.a0 as usize;
    let rlim = args.a1 as *mut RLimit;
    if resource >= sched::task::RLIM_NLIMITS {
        return -(EINVAL as i64);
    }
    if rlim.is_null() {
        return -(EFAULT as i64);
    }
    let nr = sched::current_index();
    let r = unsafe { (*sched::task_ptr(nr)).rlim[resource] };
    // SAFETY: 用户指针，rlimit 是 POD。
    unsafe { (*rlim).rlim_cur = r.rlim_cur; (*rlim).rlim_max = r.rlim_max; }
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
    let n = len.min(256);
    unsafe {
        // 伪随机：rdtsc + 静态计数做种子，xorshift 展开。glibc mkstemps 靠
        // 这个生成临时文件后缀，若恒返回 0（旧实现），gcc 每次都用同一个
        // 临时名，第二次编译就 EEXIST「File exists」。
        let mut lo: u32 = 0; let mut hi: u32 = 0;
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
        static mut CTR: u64 = 0x9E3779B97F4A7C15;
        let mut s = ((hi as u64) << 32 | lo as u64).wrapping_add(*core::ptr::addr_of!(CTR));
        *core::ptr::addr_of_mut!(CTR) = (*core::ptr::addr_of!(CTR)).wrapping_add(0x9E3779B97F4A7C15);
        for i in 0..n {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            *buf.add(i) = (s >> 32) as u8;
        }
    }
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

/// 给一个匿名事件对象分配一个空闲 fd 并绑定。返回 fd，失败 -EMFILE。
fn alloc_event_fd(obj_idx: usize) -> i64 {
    for fd in 3usize..64 {
        if !crate::fs::pipe::fd_is_pipe(fd)
            && !crate::net::socket::fd_is_socket(fd)
            && !crate::fs::event::fd_is_event(fd)
            && unsafe { crate::fs::open::fd_to_filp(fd) == crate::fs::inode::NIL }
        {
            crate::fs::event::register_fd(fd, obj_idx);
            return fd as i64;
        }
    }
    -(crate::klib::errno::EMFILE as i64)
}

/// inotify.
pub fn inotify_init(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let idx = crate::fs::event::inotify_init(0);
    if idx < 0 { return idx; }
    alloc_event_fd(idx as usize)
}
pub fn inotify_init1(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let idx = crate::fs::event::inotify_init(args.a0 as u32);
    if idx < 0 { return idx; }
    alloc_event_fd(idx as usize)
}
pub fn inotify_add_watch(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::event::inotify_add_watch(args.a0 as usize, args.a1, args.a2 as u32)
}
pub fn inotify_rm_watch(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::event::inotify_rm_watch(args.a0 as usize, args.a1 as u32)
}

/// epoll.
pub fn epoll_create(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let idx = crate::fs::event::epoll_create(0);
    if idx < 0 { return idx; }
    alloc_event_fd(idx as usize)
}
pub fn epoll_create1(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let idx = crate::fs::event::epoll_create(args.a0 as u32);
    if idx < 0 { return idx; }
    alloc_event_fd(idx as usize)
}
pub fn epoll_ctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::event::epoll_ctl(args.a0 as usize, args.a1 as u32, args.a2 as usize, args.a3)
}
pub fn epoll_wait(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::event::epoll_wait(args.a0 as usize, args.a1, args.a2 as u32, args.a3 as i32)
}

/// timerfd.
pub fn timerfd_create(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let idx = crate::fs::event::timerfd_create(args.a0 as u32, args.a1 as u32);
    if idx < 0 { return idx; }
    alloc_event_fd(idx as usize)
}
pub fn timerfd_settime(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::event::timerfd_settime(args.a0 as usize, args.a1 as u32, args.a2, args.a3)
}
pub fn timerfd_gettime(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::event::timerfd_gettime(args.a0 as usize, args.a1)
}

/// eventfd.
pub fn eventfd(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // eventfd(initval)：无 flags。
    let idx = crate::fs::event::eventfd_create(args.a0 as u32, 0);
    if idx < 0 { return idx; }
    alloc_event_fd(idx as usize)
}
pub fn eventfd2(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // eventfd2(initval, flags)。
    let idx = crate::fs::event::eventfd_create(args.a0 as u32, args.a1 as u32);
    if idx < 0 { return idx; }
    alloc_event_fd(idx as usize)
}

/// file operations.
/// 零拷贝管道搬运。`splice(fd_in, off_in*, fd_out, off_out*, len, flags)`。
/// 至少一端必须是管道；经内核页缓冲在管道与文件/设备之间搬运（非阻塞）。
pub fn splice(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::mm::{get_free_page, free_page, PAGE_SIZE};
    let fd_in = args.a0 as usize;
    let off_in = args.a1;
    let fd_out = args.a2 as usize;
    let off_out = args.a3;
    let len = args.a4 as u64;
    let _flags = args.a5 as u32;

    if len == 0 {
        return 0;
    }
    // 本树 splice 不支持 socket 端点。
    if crate::net::socket::fd_is_socket(fd_in) || crate::net::socket::fd_is_socket(fd_out) {
        return -(EINVAL as i64);
    }
    let in_pipe = crate::fs::pipe::fd_is_pipe(fd_in);
    let out_pipe = crate::fs::pipe::fd_is_pipe(fd_out);
    if !in_pipe && !out_pipe {
        return -(EINVAL as i64);
    }
    let pin = if in_pipe { crate::fs::pipe::fd_to_pipe(fd_in) } else { None };
    let pout = if out_pipe { crate::fs::pipe::fd_to_pipe(fd_out) } else { None };

    // 起始偏移：管道侧忽略 off；文件侧 NULL 用 f_pos，否则读 *off。
    let mut in_pos: i64 = if in_pipe {
        0
    } else if off_in != 0 {
        if !check_range(off_in, 8) { return -(EFAULT as i64); }
        // SAFETY: 已校验可读。
        unsafe { core::ptr::read_unaligned(off_in as *const i64) }
    } else {
        let f = crate::fs::open::fd_to_filp(fd_in);
        if f == crate::fs::inode::NIL { return -(EBADF as i64); }
        // SAFETY: f 有效。
        unsafe { crate::fs::file_table::filp(f).f_pos as i64 }
    };
    let mut out_pos: i64 = if out_pipe {
        0
    } else if off_out != 0 {
        if !check_range(off_out, 8) { return -(EFAULT as i64); }
        // SAFETY: 已校验可读。
        unsafe { core::ptr::read_unaligned(off_out as *const i64) }
    } else {
        let f = crate::fs::open::fd_to_filp(fd_out);
        if f == crate::fs::inode::NIL { return -(EBADF as i64); }
        // SAFETY: f 有效。
        unsafe { crate::fs::file_table::filp(f).f_pos as i64 }
    };
    if in_pos < 0 || out_pos < 0 {
        return -(EINVAL as i64);
    }

    let buf = get_free_page();
    if buf == 0 {
        return -(crate::klib::errno::ENOMEM as i64);
    }

    let mut total: i64 = 0;
    let mut remaining = len;
    while remaining > 0 {
        let chunk = core::cmp::min(remaining as usize, PAGE_SIZE);
        // 读源。
        let n: i64 = if in_pipe {
            crate::fs::pipe::pipe_read_kernel(pin.unwrap(), buf as *mut u8, chunk)
        } else {
            // SAFETY: buf 是本页大小内核内存。
            unsafe {
                let dest = core::slice::from_raw_parts_mut(buf as *mut u8, chunk);
                let saved = crate::fs::read_write::lseek(fd_in, 0, crate::fs::SEEK_CUR);
                crate::fs::read_write::lseek(fd_in, in_pos, crate::fs::SEEK_SET);
                let r = crate::fs::read_write::read(fd_in, dest);
                crate::fs::read_write::lseek(fd_in, saved, crate::fs::SEEK_SET);
                r
            }
        };
        if n <= 0 {
            break;
        }
        let n = n as usize;
        // 写目标。
        let w: i64 = if out_pipe {
            crate::fs::pipe::pipe_write_kernel(pout.unwrap(), buf as *const u8, n)
        } else {
            // SAFETY: buf 是本页大小内核内存。
            unsafe {
                let src = core::slice::from_raw_parts(buf as *const u8, n);
                let saved = crate::fs::read_write::lseek(fd_out, 0, crate::fs::SEEK_CUR);
                crate::fs::read_write::lseek(fd_out, out_pos, crate::fs::SEEK_SET);
                let r = crate::fs::read_write::write(fd_out, src);
                crate::fs::read_write::lseek(fd_out, saved, crate::fs::SEEK_SET);
                r
            }
        };
        if w <= 0 {
            break;
        }
        let w = w as usize;
        total += w as i64;
        if !in_pipe { in_pos += w as i64; }
        if !out_pipe { out_pos += w as i64; }
        if (w as u64) < remaining {
            break;
        }
        remaining -= w as u64;
    }
    free_page(buf);

    // 回写偏移 / 推进 f_pos（管道侧无需处理）。
    if !in_pipe {
        if off_in != 0 {
            // SAFETY: 已校验可写。
            unsafe { core::ptr::write_unaligned(off_in as *mut i64, in_pos); }
        } else {
            unsafe { crate::fs::read_write::lseek(fd_in, in_pos, crate::fs::SEEK_SET); }
        }
    }
    if !out_pipe {
        if off_out != 0 {
            // SAFETY: 已校验可写。
            unsafe { core::ptr::write_unaligned(off_out as *mut i64, out_pos); }
        } else {
            unsafe { crate::fs::read_write::lseek(fd_out, out_pos, crate::fs::SEEK_SET); }
        }
    }
    total
}

/// 复制管道数据（两端都必须是管道，源**不消费**）。`tee(fd_in, fd_out, len, flags)`。
pub fn tee(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::mm::{get_free_page, free_page, PAGE_SIZE};
    let fd_in = args.a0 as usize;
    let fd_out = args.a1 as usize;
    let len = args.a2 as u64;
    let _flags = args.a3 as u32;

    if len == 0 {
        return 0;
    }
    if !crate::fs::pipe::fd_is_pipe(fd_in) || !crate::fs::pipe::fd_is_pipe(fd_out) {
        return -(EINVAL as i64);
    }
    let pin = crate::fs::pipe::fd_to_pipe(fd_in).unwrap();
    let pout = crate::fs::pipe::fd_to_pipe(fd_out).unwrap();

    let buf = get_free_page();
    if buf == 0 {
        return -(crate::klib::errno::ENOMEM as i64);
    }
    // 单次 peek + write：源不消费，所以一次 tee 复制「从头数 len 字节」。
    let chunk = core::cmp::min(len as usize, PAGE_SIZE);
    let n = crate::fs::pipe::pipe_peek_kernel(pin, buf as *mut u8, chunk);
    let mut total: i64 = 0;
    if n > 0 {
        let w = crate::fs::pipe::pipe_write_kernel(pout, buf as *const u8, n as usize);
        total = if w > 0 { w } else { 0 };
    }
    free_page(buf);
    total
}

/// 把用户内存 iovec 搬进管道。`vmsplice(fd, iov, nr_segs, flags)`。fd 必须是管道。
pub fn vmsplice(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as usize;
    let iov = args.a1 as *const IoVec;
    let nr_segs = args.a2 as usize;
    let _flags = args.a3 as u32;

    if !crate::fs::pipe::fd_is_pipe(fd) {
        return -(EINVAL as i64);
    }
    let pidx = crate::fs::pipe::fd_to_pipe(fd).unwrap();
    if iov.is_null() || nr_segs == 0 {
        return 0;
    }
    if nr_segs > 1024 {
        return -(EINVAL as i64);
    }

    let mut total: i64 = 0;
    for i in 0..nr_segs {
        // SAFETY: 缺 verify_area，同 readv/writev 的限制。
        let v = unsafe { &*iov.add(i) };
        if v.len == 0 {
            continue;
        }
        // 用户内存 → 管道：复用 pipe_write（内部 copy_from_user）。
        let r = crate::fs::pipe::pipe_write(pidx, v.base as *const u8, v.len as usize);
        if r < 0 {
            return if total > 0 { total } else { r };
        }
        total += r;
        if (r as u64) < v.len {
            break; // 管道满
        }
    }
    total
}
pub fn sync_file_range(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // sync_file_range(fd, offset, nbytes, flags)。无页缓存/区间写回，
    // 退化为整文件 fsync。
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: fs 层校验 fd。
    unsafe { crate::fs::read_write::fsync(fd as usize) }
}
pub fn vhangup(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn dup3(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // dup3(oldfd, newfd, flags)。glibc 的 dup2 在 x86_64 上直接走 dup3(fd,fd2,0)，
    // 所以管道重定向（bash `echo hi | cat`）全靠它。
    let (old, new) = (args.a0 as i64, args.a1 as i64);
    let flags = args.a2 as u64;
    if old < 0 || new < 0 {
        return -(EBADF as i64);
    }
    // 只允许 0 或 O_CLOEXEC(0x80000)。内核不追踪 close-on-exec 位，flags 只做校验。
    if flags != 0 && flags != 0x80000 {
        return -(EINVAL as i64);
    }
    // dup3 语义：oldfd == newfd 返回 EINVAL（dup2 直接返回 newfd）。
    if old == new {
        return -(EINVAL as i64);
    }
    // SAFETY: 契约转交 sys_dup2，fs 层校验 fd。
    unsafe { crate::fs::open::sys_dup2(old as usize, new as usize) }
}
pub fn faccessat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // faccessat(dirfd, path, mode, flags)
    let path = match unsafe { at_path(args.a0 as i64, args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    let mode = args.a2 as u16;
    let flags = args.a3 as u64;
    // AT_EACCESS = 0x200：用有效 uid/gid 而非真实 uid/gid。
    let use_effective = flags & 0x200 != 0;
    // SAFETY: 进程上下文。
    unsafe { crate::fs::open::sys_access(path, mode, use_effective) }
}
pub fn statfs(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn fstatfs(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn truncate64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn ftruncate64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fallocate(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // fallocate(fd, mode, offset, len)
    let fd = args.a0 as i64;
    let mode = args.a1 as u32;
    let offset = args.a2 as i64;
    let len = args.a3 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // 支持：mode 0（默认分配）+ FALLOC_FL_KEEP_SIZE(1)。
    if mode & !1 != 0 {
        return -(EINVAL as i64);
    }
    if offset < 0 || len < 0 {
        return -(EINVAL as i64);
    }
    let end = offset + len;
    if mode & 1 != 0 {
        // KEEP_SIZE：不改变文件大小（无预分配，直接成功）
        return 0;
    }
    // 默认：文件大小扩展到 end（若当前更大则不变）。
    let f = crate::fs::open::fd_to_filp(fd as usize);
    if f == crate::fs::inode::NIL {
        return -(EBADF as i64);
    }
    // SAFETY: f 有效。
    let n = unsafe { (*crate::fs::file_table::filp(f)).f_inode };
    if n == crate::fs::inode::NIL {
        return -(EBADF as i64);
    }
    // SAFETY: n 被打开文件持有。
    let cur = unsafe { (*crate::fs::inode::inode_ptr(n)).i_size as i64 };
    if end > cur {
        // SAFETY: fs 层校验 fd；i_size 为 u32，超界截断。
        unsafe { crate::fs::open::sys_ftruncate(fd as usize, end as u32) }
    } else {
        0
    }
}
pub fn fanotify_init(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fanotify_mark(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn copy_file_range(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::mm::{get_free_page, free_page, PAGE_SIZE};
    // copy_file_range(fd_in, off_in, fd_out, off_out, len, flags)
    let in_fd = args.a0 as usize;
    let off_in = args.a1;
    let out_fd = args.a2 as usize;
    let off_out = args.a3;
    let len = args.a4 as u64;
    let _flags = args.a5 as u32;

    if in_fd == out_fd {
        return -(EINVAL as i64);
    }
    if len == 0 {
        return 0;
    }

    // 起始偏移：off==NULL 用当前 f_pos，否则读 *off。
    let mut in_pos: i64 = if off_in != 0 {
        if !check_range(off_in, 8) { return -(EFAULT as i64); }
        // SAFETY: 已校验可读。
        unsafe { core::ptr::read_unaligned(off_in as *const i64) }
    } else {
        let f = crate::fs::open::fd_to_filp(in_fd);
        if f == crate::fs::inode::NIL { return -(EBADF as i64); }
        // SAFETY: f 有效。
        unsafe { crate::fs::file_table::filp(f).f_pos as i64 }
    };
    let mut out_pos: i64 = if off_out != 0 {
        if !check_range(off_out, 8) { return -(EFAULT as i64); }
        // SAFETY: 已校验可读。
        unsafe { core::ptr::read_unaligned(off_out as *const i64) }
    } else {
        let f = crate::fs::open::fd_to_filp(out_fd);
        if f == crate::fs::inode::NIL { return -(EBADF as i64); }
        // SAFETY: f 有效。
        unsafe { crate::fs::file_table::filp(f).f_pos as i64 }
    };
    if in_pos < 0 || out_pos < 0 {
        return -(EINVAL as i64);
    }

    let buf = get_free_page();
    if buf == 0 {
        return -(crate::klib::errno::ENOMEM as i64);
    }

    // 统一用显式定位读/写：每次 lseek 到 in_pos/out_pos，完事恢复原 f_pos。
    // off==NULL 的「推进 f_pos」语义在循环结束后统一补 lseek。
    let mut copied: u64 = 0;
    let mut remaining = len;
    while remaining > 0 {
        let chunk = core::cmp::min(remaining as usize, PAGE_SIZE);
        // SAFETY: buf 是本页大小的已分配内存。
        let dest = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, chunk) };
        let n = unsafe {
            let saved = crate::fs::read_write::lseek(in_fd, 0, crate::fs::SEEK_CUR);
            crate::fs::read_write::lseek(in_fd, in_pos, crate::fs::SEEK_SET);
            let r = crate::fs::read_write::read(in_fd, dest);
            crate::fs::read_write::lseek(in_fd, saved, crate::fs::SEEK_SET);
            r
        };
        if n <= 0 {
            break;
        }
        let n = n as usize;
        let w = unsafe {
            let saved = crate::fs::read_write::lseek(out_fd, 0, crate::fs::SEEK_CUR);
            crate::fs::read_write::lseek(out_fd, out_pos, crate::fs::SEEK_SET);
            let r = crate::fs::read_write::write(out_fd, &dest[..n]);
            crate::fs::read_write::lseek(out_fd, saved, crate::fs::SEEK_SET);
            r
        };
        if w <= 0 {
            break;
        }
        let w = w as usize;
        copied += w as u64;
        in_pos += w as i64;
        out_pos += w as i64;
        if (w as u64) < remaining {
            break;
        }
        remaining -= w as u64;
    }
    free_page(buf);

    // 回写偏移 / 推进 f_pos。
    if off_in != 0 {
        // SAFETY: 已校验可写。
        unsafe { core::ptr::write_unaligned(off_in as *mut i64, in_pos); }
    } else {
        unsafe { crate::fs::read_write::lseek(in_fd, in_pos, crate::fs::SEEK_SET); }
    }
    if off_out != 0 {
        // SAFETY: 已校验可写。
        unsafe { core::ptr::write_unaligned(off_out as *mut i64, out_pos); }
    } else {
        unsafe { crate::fs::read_write::lseek(out_fd, out_pos, crate::fs::SEEK_SET); }
    }
    copied as i64
}
pub fn preadv2(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    // preadv2(fd, iov, iovcnt, offset, flags)。忽略 flags，退化为 preadv。
    preadv(args, regs)
}
pub fn pwritev2(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    // pwritev2(fd, iov, iovcnt, offset, flags)。忽略 flags，退化为 pwritev。
    pwritev(args, regs)
}
pub fn statx(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // statx(dirfd, pathname, flags, mask, statxbuf)
    let dirfd = args.a0 as i32;
    let path_ptr = args.a1;
    let _flags = args.a2 as u32;
    let _mask = args.a3 as u32;
    let buf = args.a4;

    if buf == 0 {
        return -(EFAULT as i64);
    }

    // 空路径 / AT_EMPTY_PATH：fstat(dirfd)。
    let path_empty = path_ptr != 0
        && check_range(path_ptr, 1)
        && unsafe { core::ptr::read_volatile(path_ptr as *const u8) } == 0;

    let inr = if path_ptr != 0 && !path_empty && dirfd == -100 {
        // AT_FDCWD + 非空路径：按路径 stat
        match unsafe { user_path(path_ptr) } {
            Ok(p) => match unsafe { crate::fs::namei::namei(p) } {
                Ok(n) => n,
                Err(e) => return e as i64,
            },
            Err(e) => return e,
        }
    } else if path_ptr == 0 || path_empty {
        let filp_idx = crate::fs::open::fd_to_filp(dirfd as usize);
        if filp_idx == crate::fs::inode::NIL {
            return -(EBADF as i64);
        }
        // SAFETY: filp_idx 有效
        unsafe { (*crate::fs::file_table::filp(filp_idx)).f_inode }
    } else {
        return -(EINVAL as i64);
    };

    let need = core::mem::size_of::<crate::fs::stat::Statx>() as u64;
    if !check_range(buf, need) {
        // SAFETY: inr 是已 iget 的下标。
        unsafe { crate::fs::inode::iput(inr); }
        return -(EFAULT as i64);
    }
    let mut s = crate::fs::stat::Stat64::zeroed();
    // SAFETY: inr 是有效 inode 下标。
    unsafe { crate::fs::stat::cp_new_stat(inr, &mut s); }
    unsafe { crate::fs::inode::iput(inr); }
    let x = crate::fs::stat::Statx::from_stat64(&s);
    // SAFETY: check_range 通过。
    unsafe { core::ptr::write_unaligned(buf as *mut crate::fs::stat::Statx, x) };
    0
}
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

/// extended attributes：无 xattr 支持。返回 `-EOPNOTSUPP`（而非 `-ENOSYS`），
/// 这样 libselinux/libacl 会把「无 SELinux 标签/无 ACL」当作「不支持」而非
/// 硬错误——否则 `ls` 每条目录项都刷「Function not implemented」。
pub fn setxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }
pub fn lsetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }
pub fn fsetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }
pub fn getxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }
pub fn lgetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }
pub fn fgetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }
pub fn listxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }
pub fn llistxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }
pub fn flistxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }
pub fn removexattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }
pub fn lremovexattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }
pub fn fremovexattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EOPNOTSUPP as i64) }

/// io_uring (simplified stub).
pub fn io_uring_setup(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_uring_enter(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_uring_register(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// other advanced syscalls.
pub fn kexec_load(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn init_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn delete_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置调度属性。只有 SCHED_OTHER 一种策略，校验后接受（nice/priority
/// 映射到 task.priority 的语义见 setpriority）。
pub fn sched_setattr(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pid = args.a0 as i32;
    let attr = args.a1 as *const u64;
    let flags = args.a3;
    if attr.is_null() || flags != 0 { return -(EINVAL as i64); }
    // sched_attr: size(0) policy(4) flags(8) nice(12) priority(16) ...
    // SAFETY: 用户指针恒等映射
    let size = unsafe { core::ptr::read_volatile(attr as *const u32) } as usize;
    if size < 32 { return -(EINVAL as i64); }
    let policy = unsafe { core::ptr::read_volatile((attr as *const u32).add(1)) };
    if policy != 0 { return -(EINVAL as i64); } // 只支持 SCHED_OTHER
    if pid != 0 {
        // 只支持自己
        let me_pid = unsafe { (*sched::task_ptr(sched::current_index())).pid };
        if pid != me_pid { return -(ESRCH as i64); }
    }
    0
}
/// 取调度属性。报告 SCHED_OTHER + 默认参数，attr.size 回写用户给的 size。
pub fn sched_getattr(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pid = args.a0 as i32;
    let attr = args.a1 as *mut u64;
    let size = args.a2 as usize;
    let flags = args.a3;
    if attr.is_null() || flags != 0 || size < 32 { return -(EINVAL as i64); }
    if pid != 0 {
        let me_pid = unsafe { (*sched::task_ptr(sched::current_index())).pid };
        if pid != me_pid { return -(ESRCH as i64); }
    }
    // SAFETY: 用户指针恒等映射
    unsafe {
        let p = attr as *mut u32;
        core::ptr::write_volatile(p, size as u32);      // size
        core::ptr::write_volatile(p.add(1), 0);         // policy = SCHED_OTHER
        core::ptr::write_volatile(p.add(2), 0);         // flags(lo 部分)
        core::ptr::write_volatile(p.add(3), 0);         // nice = 0
        core::ptr::write_volatile(p.add(4), 0);         // priority = 0
        core::ptr::write_volatile(attr.add(3), 0);      // runtime
        core::ptr::write_volatile(attr.add(4), 0);      // deadline
        core::ptr::write_volatile(attr.add(5), 0);      // period
    }
    0
}
pub fn seccomp(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn memfd_create(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn userfaultfd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn membarrier(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 调整时钟。没有 RTC/NTP 环路，接受但忽略。
pub fn clock_adjtime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
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
/// RTC 驱动已接：startup_time（CMOS）+ jiffies/HZ。
pub fn time(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // startup_time（RTC）+ jiffies/HZ：不再是「从 0 起算的 tick 秒」
    let secs = sched::current_time() as i64;
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
/// 查询页面是否驻留。惰性分配/换页之后「全驻留」不再成立：RESERVED
/// 和 SWAPPED 叶子都算非驻留。vec 每页一个字节，bit0=驻留。
pub fn mincore(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::mm::{page, paging};
    let addr = args.a0 as usize;
    let len = args.a1 as usize;
    let vec = args.a2 as usize;
    // Linux 要求 addr 页对齐。
    if addr & (page::PAGE_SIZE - 1) != 0 {
        return -(EINVAL as i64);
    }
    let npages = (len + page::PAGE_SIZE - 1) / page::PAGE_SIZE;
    // SAFETY: vec 指针只来自用户传参，user_ok 只做范围检查。
    if !unsafe { user_ok(vec as u64, npages as u64, crate::mm::area::AccessMode::Write) } {
        return -(EFAULT as i64);
    }
    unsafe {
        let pml4 = paging::current_pml4();
        for i in 0..npages {
            let resident = match paging::leaf_entry(pml4, addr + i * page::PAGE_SIZE) {
                Some(e) => e & paging::flags::PRESENT != 0,
                None => false,
            };
            // SAFETY: vec 已校验用户可写。
            unsafe { core::ptr::write_volatile((vec + i) as *mut u8, resident as u8) };
        }
    }
    0
}
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
/// 改文件属主（不跟随符号链接）。同 [`chown`]，`follow=false`。
pub fn lchown(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let owner = args.a1 as u32;
    let group = args.a2 as u32;
    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 进程上下文。
    unsafe { crate::fs::open::sys_chown(path, owner, group, false) }
}
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
/// 设置附加组列表。需要 root；无附加组存储，root 也仅接受空列表。
pub fn setgroups(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let size = args.a0 as usize;
    // SAFETY: 进程上下文。
    unsafe {
        if suser() {
            if size == 0 { 0 } else { -(EINVAL as i64) }
        } else {
            -(EPERM as i64)
        }
    }
}
/// 设置真实/有效 uid。对应原版 `kernel/sys.c:sys_setreuid()`。
pub fn setreuid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let ruid = args.a0 as u32;
    let euid = args.a1 as u32;
    // SAFETY: 进程上下文，单核。
    unsafe {
        let c = crate::sched::current();
        let old_ruid = c.uid;
        if ruid != u32::MAX {
            if c.euid == ruid || old_ruid == ruid || c.euid == 0 {
                c.uid = ruid;
            } else {
                return -(EPERM as i64);
            }
        }
        if euid != u32::MAX {
            if old_ruid == euid || c.euid == euid || c.euid == 0 {
                c.euid = euid;
                c.suid = euid;
            } else {
                c.uid = old_ruid; // 回滚 ruid 变更（同原版）
                return -(EPERM as i64);
            }
        }
    }
    0
}
/// 设置真实/有效 gid。对应原版 `kernel/sys.c:sys_setregid()`。
pub fn setregid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let rgid = args.a0 as u32;
    let egid = args.a1 as u32;
    // SAFETY: 进程上下文，单核。
    unsafe {
        let c = crate::sched::current();
        let old_rgid = c.gid;
        if rgid != u32::MAX {
            if c.egid == rgid || old_rgid == rgid || c.euid == 0 {
                c.gid = rgid;
            } else {
                return -(EPERM as i64);
            }
        }
        if egid != u32::MAX {
            if old_rgid == egid || c.egid == egid || c.euid == 0 {
                c.egid = egid;
                c.sgid = egid;
            } else {
                c.gid = old_rgid; // 回滚 rgid 变更（同原版）
                return -(EPERM as i64);
            }
        }
    }
    0
}
/// 设置真实/有效/保存 uid。`-1` 表示保持不变。
pub fn setresuid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let ruid = args.a0 as u32;
    let euid = args.a1 as u32;
    let suid = args.a2 as u32;
    // SAFETY: 进程上下文，单核。
    unsafe {
        let c = crate::sched::current();
        // 非 root：新值必须落在当前 uid/euid/suid 中之一。
        if c.euid != 0 {
            for v in [ruid, euid, suid] {
                if v != u32::MAX && v != c.uid && v != c.euid && v != c.suid {
                    return -(EPERM as i64);
                }
            }
        }
        if ruid != u32::MAX { c.uid = ruid; }
        if euid != u32::MAX { c.euid = euid; }
        if suid != u32::MAX { c.suid = suid; }
        // Linux：setresuid 把 fsuid 归位到新 euid。
        c.fsuid = c.euid;
    }
    0
}
/// 设置真实/有效/保存 gid。`-1` 表示保持不变。
pub fn setresgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let rgid = args.a0 as u32;
    let egid = args.a1 as u32;
    let sgid = args.a2 as u32;
    // SAFETY: 进程上下文，单核。
    unsafe {
        let c = crate::sched::current();
        if c.euid != 0 {
            for v in [rgid, egid, sgid] {
                if v != u32::MAX && v != c.gid && v != c.egid && v != c.sgid {
                    return -(EPERM as i64);
                }
            }
        }
        if rgid != u32::MAX { c.gid = rgid; }
        if egid != u32::MAX { c.egid = egid; }
        if sgid != u32::MAX { c.sgid = sgid; }
        c.fsgid = c.egid;
    }
    0
}
/// 设置文件系统 uid，返回旧 fsuid。Linux 语义：root 或新值 ∈ {uid,euid,suid}
/// 时生效，否则保持 fsuid = euid。
pub fn setfsuid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let uid = args.a0 as u32;
    // SAFETY: 进程上下文，单核。
    unsafe {
        let c = crate::sched::current();
        let old = c.fsuid;
        if c.euid == 0 || uid == c.uid || uid == c.euid || uid == c.suid {
            c.fsuid = uid;
        }
        old as i64
    }
}
/// 设置文件系统 gid。同 [`setfsuid`]。
pub fn setfsgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let gid = args.a0 as u32;
    // SAFETY: 进程上下文，单核。
    unsafe {
        let c = crate::sched::current();
        let old = c.fsgid;
        if c.euid == 0 || gid == c.gid || gid == c.egid || gid == c.sgid {
            c.fsgid = gid;
        }
        old as i64
    }
}
/// 取真实/有效/保存 uid。
pub fn getresuid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 进程上下文，单核。
    let (r, e, s) = unsafe {
        let c = crate::sched::current();
        (c.uid, c.euid, c.suid)
    };
    put_triple(args, r, e, s)
}
/// 取真实/有效/保存 gid。同 [`getresuid`]。
pub fn getresgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 进程上下文，单核。
    let (r, e, s) = unsafe {
        let c = crate::sched::current();
        (c.gid, c.egid, c.sgid)
    };
    put_triple(args, r, e, s)
}

/// `getresuid`/`getresgid` 的公共写回。
fn put_triple(args: &SysArgs, r: u32, e: u32, s: u32) -> i64 {
    for (p, v) in [args.a0, args.a1, args.a2].into_iter().zip([r, e, s]) {
        let p = p as *mut u32;
        if p.is_null() {
            return -(EFAULT as i64);
        }
        // SAFETY: 缺 verify_area，同 sys_write 的限制。
        unsafe { p.write_volatile(v) };
    }
    0
}

/// 取进程组。a0 == 0 表示当前进程；否则按 pid 查。
pub fn getpgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pid = args.a0 as i32;
    // SAFETY: 只读任务表；系统调用上下文。
    unsafe {
        if pid == 0 {
            return (*sched::task_ptr(sched::current_index())).pgrp as i64;
        }
        for i in 0..sched::NR_TASKS {
            let t = sched::task_ptr(i);
            if (*t).state == crate::sched::task::TaskState::Unused { continue; }
            if (*t).pid == pid {
                return (*t).pgrp as i64;
            }
        }
        -(crate::klib::errno::ESRCH as i64)
    }
}

/// 取会话 ID。a0 == 0 表示当前进程；否则按 pid 查。
pub fn getsid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pid = args.a0 as i32;
    // SAFETY: 只读任务表；系统调用上下文。
    unsafe {
        if pid == 0 {
            return (*sched::task_ptr(sched::current_index())).session as i64;
        }
        for i in 0..sched::NR_TASKS {
            let t = sched::task_ptr(i);
            if (*t).state == crate::sched::task::TaskState::Unused { continue; }
            if (*t).pid == pid {
                return (*t).session as i64;
            }
        }
        -(crate::klib::errno::ESRCH as i64)
    }
}

/// 设资源限制。对应原版 `kernel/sys.c:sys_setrlimit()`：
/// 软限不能超过硬限，抬硬限要 root。限制本身存进 task 的 rlim 表，
/// 由 brk/mmap/fork 等使用方查表强制。
pub fn setrlimit(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let resource = args.a0 as usize;
    let rlim = args.a1 as *const RLimit;
    if resource >= sched::task::RLIM_NLIMITS {
        return -(EINVAL as i64);
    }
    if rlim.is_null() {
        return -(EFAULT as i64);
    }
    let nr = sched::current_index();
    let t = unsafe { sched::task_ptr(nr) };
    // SAFETY: 用户指针，rlimit 是 POD。
    let new = unsafe { ((*rlim).rlim_cur, (*rlim).rlim_max) };
    let old = unsafe { (*t).rlim[resource] };
    if new.0 > new.1 {
        return -(EINVAL as i64);
    }
    // 抬硬限或软限超过旧硬限都要特权（原版 suser()）
    if new.1 > old.rlim_max || new.0 > old.rlim_max {
        if unsafe { (*t).euid } != 0 {
            return -(EPERM as i64);
        }
    }
    unsafe { (*t).rlim[resource] = sched::task::Rlimit { rlim_cur: new.0, rlim_max: new.1 }; }
    0
}
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
/// 指定偏移读，不改 f_pos。对应 `sys_pread64()`。
///
/// glibc 的动态链接器 `_dl_map_object_from_fd` 用 `__pread64_nocancel` 读
/// 共享库的 ELF 头/程序头——不是从 f_pos 0 读，而是带偏移的定位读。旧存根
/// 返回 0，ld.so 读到空数据 → 「cannot read file data」。
pub fn pread64(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    let fd = args.a0;
    let offset = args.a3;
    // 保存当前 f_pos，读完恢复（POSIX：pread 不改变文件偏移）。
    let old_pos = unsafe { crate::fs::read_write::lseek(fd as usize, 0, crate::fs::SEEK_CUR) };
    unsafe { crate::fs::read_write::lseek(fd as usize, offset as i64, crate::fs::SEEK_SET); }
    let sub = SysArgs { a0: fd, a1: args.a1, a2: args.a2, a3: 0, a4: 0, a5: 0 };
    let r = read(&sub, regs);
    unsafe { crate::fs::read_write::lseek(fd as usize, old_pos, crate::fs::SEEK_SET); }
    r
}
/// 指定偏移写，不改 f_pos。对应 `sys_pwrite64()`。同 [`pread64`] 的 f_pos 保存/恢复。
pub fn pwrite64(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    let fd = args.a0;
    let offset = args.a3;
    let old_pos = unsafe { crate::fs::read_write::lseek(fd as usize, 0, crate::fs::SEEK_CUR) };
    unsafe { crate::fs::read_write::lseek(fd as usize, offset as i64, crate::fs::SEEK_SET); }
    let sub = SysArgs { a0: fd, a1: args.a1, a2: args.a2, a3: 0, a4: 0, a5: 0 };
    let r = write(&sub, regs);
    unsafe { crate::fs::read_write::lseek(fd as usize, old_pos, crate::fs::SEEK_SET); }
    r
}
/// 内核内文件到文件的搬运。需要 fs 层的 splice 基础设施。
pub fn sendfile(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::{EBADF, EINVAL, ENOMEM, EOVERFLOW};
    use crate::mm::{get_free_page, free_page, PAGE_SIZE};

    let out_fd = args.a0 as usize;
    let in_fd = args.a1 as usize;
    let off_ptr = args.a2;
    let count = args.a3;

    if in_fd == out_fd {
        return -(EINVAL as i64);
    }

    let mut fpos: i64;
    let mut update_fpos = false;
    if off_ptr != 0 {
        if !check_range(off_ptr, 8) {
            return -(EINVAL as i64);
        }
        // SAFETY: check_range 已确认可读。
        fpos = unsafe { core::ptr::read_unaligned(off_ptr as *const i64) };
        if fpos < 0 {
            return -(EINVAL as i64);
        }
    } else {
        // 用 in_fd 当前 f_pos，读完推进
        let f = crate::fs::open::fd_to_filp(in_fd);
        if f == crate::fs::inode::NIL {
            return -(EBADF as i64);
        }
        // SAFETY: f 有效
        fpos = unsafe { crate::fs::file_table::filp(f).f_pos } as i64;
        update_fpos = true;
    }

    let buf = get_free_page();
    if buf == 0 {
        return -(ENOMEM as i64);
    }

    let mut sent: i64 = 0;
    let mut remaining = count;
    while remaining > 0 {
        let chunk = core::cmp::min(remaining as usize, PAGE_SIZE);
        let dest = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, chunk) };
        // SAFETY: 系统调用上下文，read 会睡。
        let n = if off_ptr != 0 {
            // 偏移读：lseek 到 fpos 再读，读完恢复（in_fd 的 f_pos 可能被别处用）
            unsafe {
                let saved = crate::fs::read_write::lseek(in_fd, 0, crate::fs::SEEK_CUR);
                crate::fs::read_write::lseek(in_fd, fpos, crate::fs::SEEK_SET);
                let r = crate::fs::read_write::read(in_fd, dest);
                crate::fs::read_write::lseek(in_fd, saved, crate::fs::SEEK_SET);
                r
            }
        } else {
            unsafe { crate::fs::read_write::read(in_fd, dest) }
        };
        if n <= 0 {
            break;
        }
        let n = n as usize;
        // SAFETY: 同上。
        let w = unsafe { crate::fs::read_write::write(out_fd, &dest[..n]) };
        if w <= 0 {
            break;
        }
        let w = w as usize;
        sent += w as i64;
        fpos += w as i64;
        if (w as u64) < remaining {
            // out_fd 没接住全部，停
            remaining = 0;
        } else {
            remaining -= w as u64;
        }
        if fpos < 0 {
            sent = -(EOVERFLOW as i64);
            break;
        }
    }

    free_page(buf);

    if update_fpos {
        let f = crate::fs::open::fd_to_filp(in_fd);
        if f != crate::fs::inode::NIL {
            // SAFETY: f 有效
            unsafe { crate::fs::file_table::filp(f).f_pos = fpos as u64 };
        }
    }
    if off_ptr != 0 && sent >= 0 {
        // SAFETY: check_range 已确认可写。
        unsafe { core::ptr::write_unaligned(off_ptr as *mut i64, fpos) };
    }
    sent
}
/// 创建进程/线程。`sys_fork` 已有，clone 的 flags 语义（共享地址空间/文件表）还没有。
/// clone syscall — 创建进程/线程。flags 控制资源共享。
pub fn clone(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::EAGAIN;
    use crate::sched::task::{STACK_MAGIC, TaskState};

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

    // 内核栈从预划的池里取（连续、对齐）。不能用 get_free_page 单页：
    // KSTACK_SIZE 是 4 页，单页分配会让第 2..4 页与别的 get_free_page 分配
    // 重叠，clone 的 pt_regs 写会覆盖掉用户栈（bug：posix_spawn 子栈读垃圾）。
    let stack_page = unsafe {
        let f = crate::irq::local_irq_save();
        let s = sched::alloc_kstack();
        crate::irq::restore_flags(f);
        s
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
        // fork/clone 不继承间隔定时器（fork(2)：「the child ... its interval
        // timers are reset」），rlim 则随结构体复制继承。
        (*child).it_real_value = 0; (*child).it_real_incr = 0;
        (*child).it_virt_value = 0; (*child).it_virt_incr = 0;
        (*child).it_prof_value = 0; (*child).it_prof_incr = 0;
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
        // file-backed mmap 的 VMA 记账随地址空间克隆（CLONE_VM 共享页表的
        // 线程也复制一份，保证两边的缺页解析行为一致）。
        if (*child).pml4 != 0 {
            crate::mm::mmap_vma::clone_table(parent_nr, child_nr);
        }

        // CLONE_THREAD: threads get unique PID, share address space via CLONE_VM
        if flags & CLONE_THREAD != 0 {
            (*child).pid = sched::allocate_pid();
            // Thread group: parent pid stays as the "tgid" equivalent
        } else {
            (*child).pid = sched::allocate_pid();
            (*child).parent = parent_nr;
        }

        // CLONE_VFORK：挂起父进程，直到子进程 execve 或退出。glibc 的
        // posix_spawn(system) 用 CLONE_VM|CLONE_VFORK：子进程在父进程的
        // 地址空间里跑，父进程必须停住，否则父进程 __spawnix 里立即 munmap
        // 子栈会把子进程还在用的栈页卸掉。
        if flags & CLONE_VFORK != 0 {
            (*child).vfork_parent = parent_nr;
            (*parent).state = TaskState::Interruptible;
        }

        if flags & CLONE_PARENT != 0 {
            (*child).parent = (*parent).parent;
        }

        // CLONE_FILES: 线程共享 fd 表。我们的 fd 表是 per-task 旁路数组，
        // 没有真正的共享语义，所以无论是否 CLONE_FILES 都把父表复制给子进程。
        // 关键：fork 语义（clone(SIGCHLD)，不带 CLONE_FILES）必须复制 fd 表，
        // 否则子进程没有 stdin/stdout/stderr，cat 等外部命令 write(1) → EBADF。
        // 每个被继承的打开文件表项 f_count++（原版 copy_process 的语义）。
        crate::fs::open::clone_fds(parent_nr, child_nr);
        crate::fs::pipe::clone_pipe_fds(parent_nr, child_nr);
        for fd in 0..crate::fs::NR_OPEN {
            let fi = crate::fs::open::task_fd(child_nr, fd);
            if fi != crate::fs::inode::NIL {
                // SAFETY: fi 是有效的 file_table 下标。
                (*crate::fs::file_table::filp(fi)).f_count += 1;
            }
        }
        let _ = flags & CLONE_FILES;

        // CLONE_SETTLS: 为新线程设 TLS（FS base）。
        // 不能在这里 wrmsr——clone 运行在父进程上下文，wrmsr 会把父进程的
        // FS_BASE 改成子进程的 TLS 地址，立即破坏父进程的 TLS 访问。正确
        // 做法：把 new_tls 存进子进程的 fs_base 字段，等 switch_to 切到子进程
        // 时由它写 MSR_FS_BASE（与 arch_prctl/ret_from_fork 路径一致）。
        if flags & CLONE_SETTLS != 0 && new_tls != 0 {
            (*child).fs_base = new_tls;
        }

        // CLONE_CHILD_CLEARTID / CLONE_CHILD_SETTID：不能在 clone 的父进程
        // 上下文里写 child_tidptr——fork 后该页是父子共享的 COW 只读页，
        // 在父进程里写会改穿共享物理页（WP=0 时）或触发父进程的 COW（WP=1
        // 时），两种都把子进程的 tid 写进了父进程的数据，腐败父进程的堆。
        // Linux 的做法是把地址存进 task，由子进程在 ret_from_fork 里自己
        // put_user（写到自己的地址空间，COW 正确）。这里记下地址，CLEARTID
        // 的清零则留给 do_exit 将来实现。
        (*child).set_child_tid = if flags & CLONE_CHILD_SETTID != 0 && child_tidptr != 0 {
            child_tidptr
        } else {
            0
        };

        // CLONE_PARENT_SETTID: 在父进程的 parent_tidptr 处写入子进程 tid。
        // 这里写的是父进程自己的页（当前 CR3 就是父进程），不涉及 COW 穿透。
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
        let stack_top = stack_page as u64 + crate::sched::KSTACK_SIZE as u64;
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
        // 关键：把内核栈基址记进 PCB，release() 靠它 free_kstack。
        // 漏掉会继承父进程的 kernel_stack，子进程退出时 free 错槽位
        //（free 掉父进程/fsinit 的栈），下一个 clone 就拿到重叠的栈。
        (*child).kernel_stack = stack_page as u64;

        // 挂进调度环
        let old_next = (*sched::task_ptr(parent_nr)).next;
        (*sched::task_ptr(child_nr)).next = old_next;
        (*sched::task_ptr(child_nr)).prev = parent_nr;
        (*sched::task_ptr(parent_nr)).next = child_nr;
        (*sched::task_ptr(old_next)).prev = child_nr;

        let child_pid = (*child).pid as i64;
        // VFORK：父进程立刻让出 CPU，让子进程先跑（execve/退出会唤醒父进程）。
        // 不这样做父进程会在 schedule 前继续返回用户态，glibc __spawnix 立即
        // munmap 子栈，而子进程还没开始用那个栈。
        if flags & CLONE_VFORK != 0 {
            sched::schedule();
        }
        child_pid
    }
}
/// 进程跟踪。需要 `arch_ptrace` 与调试寄存器支持，未实现 → -EPERM。
pub fn ptrace(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 读内核日志环。对应原版 `kernel/printk.c:sys_syslog()`。
/// 实现 type 2/3（读全部）、4（读并清）、10（环大小）；其余需要
/// console_loglevel 管理，返回 EINVAL。
pub fn syslog(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let ty = args.a0 as i32;
    let buf = args.a1 as *mut u8;
    let len = args.a2 as usize;
    match ty {
        3 | 4 | 2 => {
            if buf.is_null() {
                return -(EFAULT as i64);
            }
            let slice = unsafe { core::slice::from_raw_parts_mut(buf, len) };
            let n = crate::klib::printk::read_log(slice);
            if ty == 4 {
                crate::klib::printk::clear_log();
            }
            n as i64
        }
        10 => crate::klib::printk::LOG_BUF_LEN as i64,
        _ => -(EINVAL as i64),
    }
}
/// 取进程能力集。本内核是单用户（uid 0）模型，相当于持有全部 capability。
/// 支持 v3（两组 32 位）与 v1（一组）布局；只查自己或已存在的进程。
pub fn capget(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    const CAP_V1: u32 = 0x1998_0330;
    const CAP_V3: u32 = 0x2008_0522;
    let hdr = args.a0 as *const u32;
    let datap = args.a1 as *mut u32;
    if hdr.is_null() {
        return -(EINVAL as i64);
    }
    // SAFETY: 系统调用上下文，用户指针恒等映射
    let (version, pid) = unsafe { (*hdr, *(hdr.add(1)) as i32) };
    if version != CAP_V1 && version != CAP_V3 {
        // 同 Linux：不识别的 version 也要回写 kernel 偏好版本号
        unsafe { *(args.a0 as *mut u32) = CAP_V3 };
        return -(EINVAL as i64);
    }
    // 校验目标进程存在（0 == 自己）
    if pid != 0 {
        let me = sched::current_index();
        let found = unsafe {
            (0..sched::NR_TASKS).any(|i| {
                let t = sched::task_ptr(i);
                (*t).state != crate::sched::task::TaskState::Unused
                    && (*t).pid as i32 == pid
            })
        };
        let _ = me;
        if !found {
            return -(ESRCH as i64);
        }
    }
    if !datap.is_null() {
        // {effective, permitted, inheritable} × 2（v3 覆盖 64 位能力）
        // SAFETY: 同上
        unsafe {
            *datap = u32::MAX;          // effective lo
            *datap.add(1) = u32::MAX;   // permitted lo
            *datap.add(2) = 0;          // inheritable lo
            if version == CAP_V3 {
                *datap.add(3) = u32::MAX; // effective hi
                *datap.add(4) = u32::MAX; // permitted hi
                *datap.add(5) = 0;        // inheritable hi
            }
        }
    }
    0
}
/// 设进程能力集。单用户模型下能力恒为全集，校验合法后空操作成功。
pub fn capset(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    if args.a0 == 0 || args.a1 == 0 {
        return -(EINVAL as i64);
    }
    0
}
/// 取待处理信号集（rt 版）。
pub fn rt_sigpending(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let set = args.a0 as *mut u64;
    let sigsetsize = args.a1;
    if sigsetsize != 8 { return -(EINVAL as i64); }
    if set.is_null() { return -(EFAULT as i64); }
    // SAFETY: 系统调用上下文，读当前任务的待处理位图
    let pending = unsafe { (*sched::task_ptr(sched::current_index())).signal };
    // SAFETY: 用户指针恒等映射
    unsafe { core::ptr::write_volatile(set, pending) };
    0
}
/// 带超时地等信号（rt 版）。对应原版 `kernel/signal.c:sys_rt_sigtimedwait()`：
/// 在 set 中有信号挂起时取出最低号信号、清除 pending 位、填 siginfo（本内核
/// 只填 si_signo/si_code=SI_USER）并返回信号号；否则睡到超时。
/// timeout=NULL 无限等；{0,0} 只查一次，无则 -EAGAIN。
pub fn rt_sigtimedwait(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let set_ptr = args.a0 as *const u64;
    let info_ptr = args.a1 as *mut i32;
    let ts_ptr = args.a2 as *const u64;
    let sigsetsize = args.a3;
    if sigsetsize != 8 { return -(EINVAL as i64); }
    if set_ptr.is_null() { return -(EFAULT as i64); }
    // SAFETY: 用户指针，读屏蔽集。位号==信号号。
    let set = unsafe { core::ptr::read_volatile(set_ptr) };
    // SIGKILL(9)/SIGSTOP(19) 不可等待
    let set = set & !((1u64 << 9) | (1u64 << 19));
    let deadline = if ts_ptr.is_null() {
        u64::MAX
    } else {
        // SAFETY: 用户指针，timespec {sec, nsec} 两个 u64。
        let (sec, nsec) = unsafe { (core::ptr::read_volatile(ts_ptr), core::ptr::read_volatile(ts_ptr.add(1))) };
        if nsec >= 1_000_000_000 { return -(EINVAL as i64); }
        let hz = sched::task::HZ as u64;
        let ticks = sec.saturating_mul(hz).saturating_add((nsec.saturating_mul(hz) + 999_999_999) / 1_000_000_000);
        sched::jiffies().saturating_add(ticks)
    };
    loop {
        let idx = sched::current_index();
        // SAFETY: 系统调用上下文读当前任务 pending 位图。
        let pending = unsafe { (*sched::task_ptr(idx)).signal } & set;
        if pending != 0 {
            let signo = pending.trailing_zeros();
            // SAFETY: 取走信号：清 pending 位（原版 dequeue_signal 语义）。
            unsafe { (*sched::task_ptr(idx)).signal &= !(1u64 << signo); }
            if !info_ptr.is_null() {
                // siginfo_t 头两个字段：si_signo / si_errno，si_code 在第 3 个
                // SAFETY: 用户指针，写 3 个 i32。
                unsafe {
                    core::ptr::write_volatile(info_ptr, signo as i32);
                    core::ptr::write_volatile(info_ptr.add(1), 0);
                    core::ptr::write_volatile(info_ptr.add(2), 0); // SI_USER
                }
            }
            return signo as i64;
        }
        if sched::jiffies() >= deadline {
            return -(EAGAIN as i64);
        }
        // SAFETY: 系统调用上下文让出 CPU，同 rt_sigsuspend 的等待路径。
        unsafe { sched::schedule() };
    }
}
/// 带 siginfo 发信号。本内核不传递 siginfo 内容（setup_frame 里 si 全 0），
/// 但按 POSIX 语义校验：用户态只允许 si_code <= 0，然后按 kill 投递。
pub fn rt_sigqueueinfo(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    sigqueue_common(args.a0 as i32, args.a1 as i32, args.a2)
}
/// rt_sigqueueinfo / rt_tgsigqueueinfo 的公共部分。
/// 线程组语义缺位，tgsigqueueinfo 的 tgid 参数按 pid 处理。
fn sigqueue_common(pid: i32, sig: i32, info: u64) -> i64 {
    if sig < 1 || sig > 31 { return -(EINVAL as i64); }
    if pid <= 0 { return -(EINVAL as i64); }
    // siginfo_t: si_signo(0) si_errno(4) si_code(8)；用户态 si_code 必须 <= 0
    if info != 0 {
        let si_code = unsafe { core::ptr::read_volatile((info as *const i32).add(2)) };
        if si_code > 0 { return -(EPERM as i64); }
    }
    let mut sent = false;
    unsafe {
        for i in 0..sched::NR_TASKS {
            let t = sched::task_ptr(i);
            if (*t).state == crate::sched::task::TaskState::Unused { continue; }
            if (*t).pid as i32 == pid {
                crate::signal::send_sig(sig as u32, i, 0);
                sent = true;
                break;
            }
        }
    }
    if sent { 0 } else { -(ESRCH as i64) }
}
/// 临时换屏蔽字并挂起，直到有未屏蔽的待处理信号。信号处理函数会在
/// 返回用户态前执行（setup_frame 路径），之后本调用总是返回 -EINTR（POSIX 语义）。
pub fn rt_sigsuspend(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let newset = args.a0 as *const u64;
    let sigsetsize = args.a1;
    if sigsetsize != 8 { return -(EINVAL as i64); }
    if newset.is_null() { return -(EFAULT as i64); }
    // SAFETY: 用户指针恒等映射
    let newmask = unsafe { core::ptr::read_volatile(newset) };
    let old = crate::signal::setsigmask(newmask);
    // 睡到有未屏蔽的待处理信号为止。合作式调度器下 schedule() 会把时间
    // 让给其他任务/中断路径，信号到达后置位 signal 即醒。
    loop {
        let pending = unsafe {
            (*sched::task_ptr(sched::current_index())).has_pending_signal()
        };
        if pending { break; }
        // SAFETY: 系统调用上下文让出 CPU，调度器契约同 vfork 等待路径
        unsafe { sched::schedule() };
    }
    crate::signal::setsigmask(old);
    -(EINTR as i64)
}
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
/// 设置执行域。只有一种 personality（PER_LINUX=0）。
/// `persona == 0xffff_ffff` 表示「查询当前 personality」。
pub fn personality(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let persona = args.a0 as u64;
    if persona == 0 || persona == 0xffff_ffff {
        0 // 始终是 PER_LINUX
    } else {
        -(EINVAL as i64)
    }
}
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
            // 三个操作符此前全写反了（SIG_BLOCK 写成覆盖、SIG_UNBLOCK 写成
            // 或、SIG_SETMASK 写成 old&~set），导致 glibc 启动时把几乎所有
            // 信号（含 SIGSEGV）都屏蔽掉，异常无法投递 → 用户态 #GP 死循环。
            // Linux 语义：BLOCK=并集，UNBLOCK=差集，SETMASK=替换。
            match how {
                0 => (*t).blocked |= set,                             // SIG_BLOCK
                1 => (*t).blocked &= !set,                            // SIG_UNBLOCK
                2 => (*t).blocked = set,                              // SIG_SETMASK
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
/// 调整系统时钟。没有 RTC 与 NTP 环路，接受但忽略。
pub fn adjtimex(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 换根目录。需要 per-task 的 root inode。
pub fn chroot(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`chdir`]。
    unsafe { crate::fs::open::sys_chroot(path) }
}
/// 开启进程记账。没有记账后台，接受但忽略。
pub fn acct(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 设置系统时间。没有 RTC，接受但忽略。
pub fn settimeofday(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 启用交换分区/设备。对应原版 `sys_swapon()`：只接受块设备
/// （`S_IFBLK`），容量按设备驱动报告的块数算；第 0 页写签名头。
pub fn swapon(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::fs::mode::S_IFMT;
    // SAFETY: user_path 已校验范围。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 进程上下文；namei 可能睡（契约允许）。
    let ino = match unsafe { crate::fs::namei::namei(path) } {
        Ok(i) => i,
        Err(e) => return e as i64,
    };
    // SAFETY: ino 有效。
    let inode = unsafe { crate::fs::inode::inode(ino) };
    if inode.i_mode & S_IFMT != crate::fs::mode::S_IFBLK {
        unsafe { crate::fs::inode::iput(ino) };
        return -(EINVAL as i64);
    }
    let dev = inode.i_rdev;
    unsafe { crate::fs::inode::iput(ino) };

    // 设备总块数（1024 字节块）。
    let major = (dev >> 8) & 0xFF;
    let minor = (dev & 0xFF) as usize;
    let blocks: u32 = if major == 1 {
        crate::drivers::block::ramdisk::RD_BLOCKS as u32
    } else if major == 3 {
        #[cfg(feature = "extra-drivers")]
        {
            (crate::drivers::block::hd::drive_size(minor) / 2) as u32
        }
        #[cfg(not(feature = "extra-drivers"))]
        {
            return -(EINVAL as i64);
        }
    } else {
        return -(EINVAL as i64);
    };
    crate::mm::swap::swapon_dev(dev, blocks)
}
/// 关闭交换分区。对应原版 `sys_swapoff()`。
pub fn swapoff(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::mm::swap::swapoff_dev()
}
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
/// 打开 POSIX 消息队列。实现见 `fs/mqueue.rs`（静态队列表，
/// mqd_t 用魔数编码，不进 fd 表）。
pub fn mq_open(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::mqueue::sys_mq_open(
        args.a0 as *const u8, args.a1 as i32, args.a2 as u32, args.a3 as *const u64)
}
/// 删除 POSIX 消息队列。
pub fn mq_unlink(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::mqueue::sys_mq_unlink(args.a0 as *const u8)
}
/// 带超时发送。
pub fn mq_timedsend(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::mqueue::sys_mq_timedsend(
        args.a0 as i64, args.a1 as *const u8, args.a2 as usize,
        args.a3 as u32, args.a4 as *const u64)
}
/// 带超时接收。
pub fn mq_timedreceive(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::mqueue::sys_mq_timedreceive(
        args.a0 as i64, args.a1 as *mut u8, args.a2 as usize,
        args.a3 as *mut u32, args.a4 as *const u64)
}
/// 注册消息到达通知（存根成功，见 fs/mqueue.rs）。
pub fn mq_notify(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::mqueue::sys_mq_notify(args.a0 as i64, args.a1)
}
/// 读写队列属性（O_NONBLOCK 可改）。
pub fn mq_getsetattr(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::fs::mqueue::sys_mq_getsetattr(
        args.a0 as i64, args.a1 as *const u64, args.a2 as *mut u64)
}
/// 等子进程（可不收尸）。`wait4` 已有，WNOWAIT 语义还没有。
pub fn waitid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设 I/O 优先级。`ll_rw_blk` 的请求队列没有优先级。
pub fn ioprio_set(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 取 I/O 优先级。同 [`ioprio_set`]。
pub fn ioprio_get(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// `fstatat` 的正式名。需要 dirfd 相对解析。
pub fn newfstatat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::{EBADF, EFAULT, EINVAL};

    let dirfd = args.a0 as i32;
    let path_ptr = args.a1;
    let stat_ptr = args.a2;
    let _flags = args.a3;

    if stat_ptr == 0 { return -(EFAULT as i64); }

    // namei / f_inode 返回的是 VFS inode 下标（已 iget），必须用
    // cp_new_stat 直接读该槽位；早先这里走 Stat64::from_inode，后者内部
    // iget(inr, 0) 把下标当超级块号、把 ino 当 0，读出磁盘 inode 0
    // （ext2 里 ino 1 才是第一个有效 inode），导致 st_mode 落到垃圾上、
    // ls 把目录当字符设备只打印名字。
    //
    // glibc 的 fstat(fd) 走 newfstatat(fd, "", st, AT_EMPTY_PATH)：
    // path 是指向 '\0' 的非空指针（空串），dirfd 是具体 fd。空串要当 fstat(dirfd)。
    // user_path 对空串返回 Err(EINVAL)，所以这里直接看首字节判空。
    let path_empty = path_ptr != 0
        && check_range(path_ptr, 1)
        && unsafe { core::ptr::read_volatile(path_ptr as *const u8) } == 0;

    let inr = if path_ptr != 0 && !path_empty && dirfd == -100 {
        // AT_FDCWD + 非空路径：按路径 stat
        let path = unsafe { user_path(path_ptr) };
        match path {
            Ok(p) => match unsafe { crate::fs::namei::namei(p) } {
                Ok(n) => n,
                Err(e) => return e as i64,
            },
            Err(e) => return e,
        }
    } else if path_ptr == 0 || path_empty {
        // NULL 路径或空串（AT_EMPTY_PATH）：fstat(dirfd)
        let filp_idx = crate::fs::open::fd_to_filp(dirfd as usize);
        if filp_idx == crate::fs::inode::NIL { return -(EBADF as i64); }
        // SAFETY: filp_idx 有效
        unsafe { (*crate::fs::file_table::filp(filp_idx)).f_inode }
    } else {
        // dirfd 非 AT_FDCWD 且路径非空：相对路径解析暂不支持
        return -(EINVAL as i64);
    };

    // SAFETY: check_range 已确认目标可写。
    if !check_range(stat_ptr, core::mem::size_of::<crate::fs::stat::Stat64>() as u64) {
        // 即便越界也要 iput，避免泄漏 inode 引用。
        // SAFETY: inr 是已 iget 的下标（namei/filp 都增了引用）。
        unsafe { crate::fs::inode::iput(inr); }
        return -(EFAULT as i64);
    }
    let mut s = crate::fs::stat::Stat64::zeroed();
    // SAFETY: inr 是有效 inode 下标。
    unsafe { crate::fs::stat::cp_new_stat(inr, &mut s); }
    unsafe { crate::fs::inode::iput(inr); }
    // SAFETY: check_range 通过。
    unsafe { core::ptr::write_unaligned(stat_ptr as *mut crate::fs::stat::Stat64, s) };
    0
}
/// 带信号屏蔽的 select。转 [`select`]，忽略 timeout/sigmask。
///
/// readline 的 `rl_getc` 在 `read()` 之前先 `pselect6` 等 fd 可读；这里复用
/// select 的「VFS 中存在的 fd 即就绪」判定，随后 read() 会阻塞在 tty_read 上，
/// 所以忽略 timeout（负值=阻塞）不影响正确性。
pub fn pselect6(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    // pselect6(nfds, readfds, writefds, exceptfds, timeout, sigmask)
    // 前 4 个参数与 select 完全相同；第 6 个是 `{sigset_t*, size}` 结构指针，忽略。
    let sel_args = SysArgs {
        a0: args.a0, a1: args.a1, a2: args.a2, a3: args.a3,
        a4: 0, a5: 0,
    };
    select(&sel_args, regs)
}
/// 带信号屏蔽的 poll。同 [`pselect6`]，转 [`poll`]。
pub fn ppoll(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    poll(args, regs)
}
/// 拆分命名空间。没有命名空间。
pub fn unshare(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 注册健壮 futex 链。同 [`futex`]。
pub fn set_robust_list(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 读健壮 futex 链。同 [`futex`]。
pub fn get_robust_list(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 带信号屏蔽的 epoll_wait。同 [`epoll_wait`]，忽略 sigmask。
pub fn epoll_pwait(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // epoll_pwait(epfd, events, maxevents, timeout, sigmask)
    crate::fs::event::epoll_wait(args.a0 as usize, args.a1, args.a2 as u32, args.a3 as i32)
}
/// 把信号变成可读的 fd。
pub fn signalfd(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // signalfd(fd, mask*, flags)
    let fd_in = args.a0 as i32;
    let mask = args.a1;
    if fd_in >= 0 {
        // 复用已有 signalfd：更新掩码。
        if !crate::fs::event::fd_is_event(fd_in as usize) {
            return -(EBADF as i64);
        }
        // SAFETY: 调用方保证 mask 可读。
        let m = unsafe { core::ptr::read_unaligned(mask as *const u64) };
        crate::fs::event::signalfd_set_mask(fd_in as usize, m);
        return fd_in as i64;
    }
    let idx = crate::fs::event::signalfd_alloc(args.a2 as u32);
    if idx < 0 { return idx; }
    let fd = alloc_event_fd(idx as usize);
    if fd >= 0 && mask != 0 {
        // SAFETY: 调用方保证 mask 可读。
        let m = unsafe { core::ptr::read_unaligned(mask as *const u64) };
        crate::fs::event::signalfd_set_mask(fd as usize, m);
    }
    fd
}
/// [`signalfd`] 的带 flags 版本。
pub fn signalfd4(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // signalfd4(fd, mask*, size, flags)
    let fd_in = args.a0 as i32;
    let mask = args.a1;
    let flags = args.a3 as u32;
    if fd_in >= 0 {
        if !crate::fs::event::fd_is_event(fd_in as usize) {
            return -(EBADF as i64);
        }
        // SAFETY: 调用方保证 mask 可读。
        let m = unsafe { core::ptr::read_unaligned(mask as *const u64) };
        crate::fs::event::signalfd_set_mask(fd_in as usize, m);
        return fd_in as i64;
    }
    let idx = crate::fs::event::signalfd_alloc(flags);
    if idx < 0 { return idx; }
    let fd = alloc_event_fd(idx as usize);
    if fd >= 0 && mask != 0 {
        // SAFETY: 调用方保证 mask 可读。
        let m = unsafe { core::ptr::read_unaligned(mask as *const u64) };
        crate::fs::event::signalfd_set_mask(fd as usize, m);
    }
    fd
}
/// 定位分散读。`preadv(fd, iov, iovcnt, offset)`，同 [`pread64`] 的 f_pos 保存/恢复。
pub fn preadv(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    let fd = args.a0;
    let offset = args.a3;
    let old_pos = unsafe { crate::fs::read_write::lseek(fd as usize, 0, crate::fs::SEEK_CUR) };
    unsafe { crate::fs::read_write::lseek(fd as usize, offset as i64, crate::fs::SEEK_SET); }
    let sub = SysArgs { a0: fd, a1: args.a1, a2: args.a2, a3: 0, a4: 0, a5: 0 };
    let r = readv(&sub, regs);
    unsafe { crate::fs::read_write::lseek(fd as usize, old_pos, crate::fs::SEEK_SET); }
    r
}
/// 定位分散写。`pwritev(fd, iov, iovcnt, offset)`，同 [`preadv`]。
pub fn pwritev(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    let fd = args.a0;
    let offset = args.a3;
    let old_pos = unsafe { crate::fs::read_write::lseek(fd as usize, 0, crate::fs::SEEK_CUR) };
    unsafe { crate::fs::read_write::lseek(fd as usize, offset as i64, crate::fs::SEEK_SET); }
    let sub = SysArgs { a0: fd, a1: args.a1, a2: args.a2, a3: 0, a4: 0, a5: 0 };
    let r = writev(&sub, regs);
    unsafe { crate::fs::read_write::lseek(fd as usize, old_pos, crate::fs::SEEK_SET); }
    r
}
/// 按 tgid/tid 带 siginfo 发信号。无线程组语义，tid 按 pid 投递。
pub fn rt_tgsigqueueinfo(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let _tgid = args.a0 as i32;
    sigqueue_common(args.a1 as i32, args.a2 as i32, args.a3)
}
/// 批量收包。对 mmsghdr 数组逐个走 [`recvmsg`]，msg_len 回写每次的字节数；
/// 中途出错时返回已成功条数（一条没收成则返回错误码），同 Linux 语义。
pub fn recvmmsg(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as usize;
    let mmsg = args.a1 as *mut u8;
    let vlen = args.a2 as u32;
    let flags = args.a3 as i32;
    // struct mmsghdr { struct msghdr msg_hdr; unsigned int msg_len; }（自然对齐 64B msghdr）
    const MMSGHDR_SIZE: usize = 64;
    if mmsg.is_null() { return -(EFAULT as i64); }
    let mut done = 0i64;
    for i in 0..vlen as usize {
        let hdr = unsafe { mmsg.add(i * MMSGHDR_SIZE) };
        let r = crate::net::socket::sys_recvmsg(fd, hdr, flags);
        if r < 0 {
            return if done > 0 { done } else { r };
        }
        // msg_len 在 msghdr 之后（对齐到 8）
        unsafe { core::ptr::write_volatile(hdr.add(56) as *mut u32, r as u32) };
        done += 1;
    }
    done
}
/// 取文件句柄。minix 层没有导出句柄的概念。
pub fn name_to_handle_at(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 按句柄打开。同 [`name_to_handle_at`]。
pub fn open_by_handle_at(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 批量发包。对 mmsghdr 数组逐个走 [`sendmsg`]，语义同 [`recvmmsg`]。
pub fn sendmmsg(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as usize;
    let mmsg = args.a1 as *const u8;
    let vlen = args.a2 as u32;
    let flags = args.a3 as i32;
    const MMSGHDR_SIZE: usize = 64;
    if mmsg.is_null() { return -(EFAULT as i64); }
    let mut done = 0i64;
    for i in 0..vlen as usize {
        let hdr = unsafe { mmsg.add(i * MMSGHDR_SIZE) };
        let r = crate::net::socket::sys_sendmsg(fd, hdr, flags);
        if r < 0 {
            return if done > 0 { done } else { r };
        }
        unsafe { core::ptr::write_volatile(hdr.add(56) as *mut u32 as *mut u32, r as u32) };
        done += 1;
    }
    done
}
/// 比较两个进程的内核资源。
pub fn kcmp(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 从 fd 装载模块。同 [`create_module`]。
pub fn finit_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 从 fd 加载 kexec 镜像。
pub fn kexec_file_load(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// BPF 系统调用。没有 BPF 虚拟机。
pub fn bpf(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 按 dirfd 执行。支持 AT_FDCWD 与绝对路径（等价 execve）；
/// dirfd 相对解析需要 fd→路径回溯，暂不支持（ENOSYS 让 glibc 回退 execve）。
pub fn execveat(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    const AT_FDCWD: i64 = -100;
    const AT_EMPTY_PATH: u64 = 0x1000;
    const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
    let dirfd = args.a0 as i64;
    let flags = args.a4;
    if flags & !(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) != 0 {
        return -(EINVAL as i64);
    }
    // 路径为空：只有 AT_EMPTY_PATH + 真 dirfd 才有意义，我们不支持
    if flags & AT_EMPTY_PATH != 0 { return -(ENOSYS as i64); }
    // 绝对路径或 AT_FDCWD：与 execve 完全等价
    let first = unsafe { core::ptr::read_volatile(args.a1 as *const u8) };
    if dirfd == AT_FDCWD || first == b'/' {
        let sub = SysArgs { a0: args.a1, a1: args.a2, a2: args.a3, a3: 0, a4: 0, a5: 0 };
        return execve(&sub, regs);
    }
    -(ENOSYS as i64)
}
/// 取进程的 pidfd。没有 pidfd 类型。
/// pidfd 编码：`0x5046_4400 | pid`（"PFD"）。不进 fd 表——pidfd 只服务
/// pidfd_* 三个系统调用，用魔数编码避免和普通 fd 混淆（语义同 mq）。
const PIDFD_MAGIC: i64 = 0x5046_4400;

/// 按 pid 找任务下标（跳过 Unused 槽位）。
fn find_task_by_pid(pid: i32) -> Option<usize> {
    // SAFETY: 系统调用上下文读任务表。
    unsafe {
        for i in 0..sched::NR_TASKS {
            let t = sched::task_ptr(i);
            if (*t).state != crate::sched::task::TaskState::Unused && (*t).pid as i32 == pid {
                return Some(i);
            }
        }
    }
    None
}

/// 打开一个进程的 pidfd。对应 Linux 5.3 的 `pidfd_open(2)`。
pub fn pidfd_open(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pid = args.a0 as i32;
    let flags = args.a1 as u32;
    if pid <= 0 || flags & !0o4000 != 0 {
        return -(EINVAL as i64);
    }
    match find_task_by_pid(pid) {
        None => -(ESRCH as i64),
        Some(_) => PIDFD_MAGIC | pid as i64,
    }
}
/// [`clone`] 的结构体参数版本（`struct clone_args`，Linux 5.3+）。
/// 把 clone_args 的关键字段映射回 clone 的五参形式：exit_signal 合入
/// flags 低字节，child stack = stack + stack_size（向下生长）。
pub fn clone3(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    // clone_args { flags, pidfd, child_tid, parent_tid, exit_signal,
    //              stack, stack_size, tls, ... } —— 每字段 u64。
    let p = args.a0 as *const u64;
    let size = args.a1 as usize;
    if p.is_null() || size < 64 {
        return -(EINVAL as i64);
    }
    // SAFETY: 用户指针已按 clone_args 布局读取前 8 个 u64。
    let (flags, child_tid, parent_tid, exit_signal, stack, stack_size, tls) = unsafe {
        (
            core::ptr::read_volatile(p),
            core::ptr::read_volatile(p.add(2)),
            core::ptr::read_volatile(p.add(3)),
            core::ptr::read_volatile(p.add(4)),
            core::ptr::read_volatile(p.add(5)),
            core::ptr::read_volatile(p.add(6)),
            core::ptr::read_volatile(p.add(7)),
        )
    };
    if exit_signal >= 64 {
        return -(EINVAL as i64);
    }
    let child_stack = if stack != 0 { stack + stack_size } else { 0 };
    let inner = SysArgs {
        a0: flags | (exit_signal & 0xFF),
        a1: child_stack,
        a2: parent_tid,
        a3: child_tid,
        a4: tls,
        a5: 0,
    };
    clone(&inner, regs)
}
/// [`faccessat`] 的带 flags 版本。
pub fn faccessat2(args: &SysArgs, regs: &mut PtRegs) -> i64 { faccessat(args, regs) }
/// [`epoll_pwait`] 的 ns 超时版本。
pub fn epoll_pwait2(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // epoll_pwait2(epfd, events, maxevents, timeout*, sigmask)。忽略 timeout/sigmask。
    crate::fs::event::epoll_wait(args.a0 as usize, args.a1, args.a2 as u32, 0)
}

// --- 正式表 424..=448（335..423 是 x32 保留段，官方 x86_64 表里没有）------------

/// 给 pidfd 发信号。没有 pidfd 类型，见 [`pidfd_open`]。
/// 经 pidfd 给进程发信号。对应 Linux 5.1 的 `pidfd_send_signal(2)`。
/// siginfo 同 rt_sigqueueinfo：只允许 si_code <= 0。
pub fn pidfd_send_signal(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pidfd = args.a0 as i64;
    let sig = args.a1 as i32;
    let info = args.a2;
    let flags = args.a3 as u32;
    if flags != 0 || pidfd & !0xFFFF != PIDFD_MAGIC {
        return -(EINVAL as i64);
    }
    if sig == 0 {
        // sig==0 只做存在性检查（同 kill(pid, 0)）
        return if find_task_by_pid((pidfd & 0xFFFF) as i32).is_some() {
            0
        } else {
            -(ESRCH as i64)
        };
    }
    if sig < 1 || sig > 31 {
        return -(EINVAL as i64);
    }
    if info != 0 {
        // SAFETY: 用户指针读 si_code（siginfo_t 第 3 个 i32）。
        let si_code = unsafe { core::ptr::read_volatile((info as *const i32).add(2)) };
        if si_code > 0 {
            return -(EPERM as i64);
        }
    }
    match find_task_by_pid((pidfd & 0xFFFF) as i32) {
        None => -(ESRCH as i64),
        Some(idx) => {
            crate::signal::send_sig(sig as u32, idx, 0);
            0
        }
    }
}
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
/// 批量关闭 fd 区间 [first, last]。对应 Linux 5.9 的 `close_range(2)`。
/// 区间内未打开的 fd 静默跳过（语义就是「保证这段全关」）。
/// flags 支持 CLOSE_RANGE_CLOEXEC（只打 close-on-exec 标记不关）和
/// CLOSE_RANGE_UNSHARE（本内核 fd 表本来就每任务私有，等价于无操作）。
pub fn close_range(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let first = args.a0 as u32;
    let last = args.a1 as u32;
    let flags = args.a2 as u32;
    if first > last {
        return -(EINVAL as i64);
    }
    const CLOSE_RANGE_UNSHARE: u32 = 2;
    const CLOSE_RANGE_CLOEXEC: u32 = 4;
    if flags & !(CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC) != 0 {
        return -(EINVAL as i64);
    }
    let last = (last as usize).min(crate::fs::NR_OPEN - 1);
    if flags & CLOSE_RANGE_CLOEXEC != 0 {
        // 不关闭，只给区间内已打开的 fd 打 close-on-exec 标记
        let nr = sched::current_index();
        for fd in first as usize..=last {
            if crate::fs::open::fd_to_filp(fd) != 0 {
                unsafe { (*sched::task_ptr(nr)).close_on_exec |= 1u64 << (fd & 63) };
            }
        }
        return 0;
    }
    // UNSHARE：本内核 fd 表本来就是每任务私有（clone_fds 全量拷贝），无需动作
    for fd in first as usize..=last {
        close_one_fd(fd);
    }
    0
}
/// 带 `open_how` 结构的 openat。需要 RESOLVE_* 解析约束。
pub fn openat2(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // openat2(dirfd, pathname, open_how*, size)
    let dirfd = args.a0 as i32;
    let path_ptr = args.a1;
    let how_ptr = args.a2;
    let size = args.a3 as usize;

    // `struct open_how` = { u64 flags; u64 mode; u64 resolve; } = 24 字节。
    if size != 24 {
        return -(EINVAL as i64);
    }
    if how_ptr == 0 || !check_range(how_ptr, 24) {
        return -(EFAULT as i64);
    }
    // SAFETY: check_range 已确认 24 字节可读。
    let (flags, mode, resolve) = unsafe {
        (
            core::ptr::read_unaligned(how_ptr as *const u64),
            core::ptr::read_unaligned((how_ptr + 8) as *const u64),
            core::ptr::read_unaligned((how_ptr + 16) as *const u64),
        )
    };
    // RESOLVE_* 位（RESOLVE_NO_SYMLINKS 等）未实现，只接受 0。
    if resolve != 0 {
        return -(EINVAL as i64);
    }
    // 退化为 openat(dirfd, path, flags, mode)。
    let sub = SysArgs { a0: dirfd as u64, a1: path_ptr, a2: flags, a3: mode, a4: 0, a5: 0 };
    openat(&sub, _regs)
}
/// 从别的进程偷一个 fd。同 [`pidfd_open`]。
/// 把另一个进程的 fd 复制进本进程。对应 Linux 5.6 的 `pidfd_getfd(2)`。
/// 等价于「远程 dup」：共享同一个打开文件表项（f_count++）。
pub fn pidfd_getfd(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pidfd = args.a0 as i64;
    let fd = args.a1 as usize;
    let flags = args.a2 as u32;
    if flags != 0 || pidfd & !0xFFFF != PIDFD_MAGIC {
        return -(EINVAL as i64);
    }
    let target = match find_task_by_pid((pidfd & 0xFFFF) as i32) {
        None => return -(ESRCH as i64),
        Some(i) => i,
    };
    let f = crate::fs::open::task_fd(target, fd);
    if f == crate::fs::inode::NIL {
        return -(EBADF as i64);
    }
    // SAFETY: 系统调用上下文；f 是目标任务持有的有效 filp 下标。
    unsafe {
        crate::fs::file_table::filp(f).f_count += 1;
        let new = crate::fs::open::get_unused_fd();
        if new == crate::fs::inode::NIL {
            crate::fs::file_table::filp(f).f_count -= 1;
            return -(crate::klib::errno::EMFILE as i64);
        }
        crate::fs::open::set_fd(new, f);
        new as i64
    }
}
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
