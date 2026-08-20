//! Minimal /proc filesystem.
//!
//! Provides virtual files that glibc and common tools expect:
//! - `/proc/self` → symlink to `/proc/<pid>`
//! - `/proc/<pid>/exe` → readlink returns exe path
//! - `/proc/<pid>/maps` → VMA listing (stub)
//! - `/proc/version` → kernel version string
//! - `/proc/meminfo` → basic memory stats
//! - `/proc/stat` → CPU stats (stub)
//! - `/proc/filesystems` → registered FS types
//!
//! Proc inodes use `data[0]` as a `ProcKind` discriminant and `data[1..3]`
//! as a u32 pid (le bytes in two u16 slots).

use crate::fs::inode::{self, FsType, Inode, NIL};
use crate::fs::mode;
use crate::fs::Dirent;
use crate::klib::errno::*;

/// What kind of proc entry this inode represents.
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProcKind {
    Root = 0,
    Version = 1,
    Meminfo = 2,
    Stat = 3,
    Filesystems = 4,
    SelfLink = 5,
    PidDir = 10,
    PidExe = 11,
    PidMaps = 12,
    PidStatus = 13,
    PidFdDir = 14,
    PidCmdline = 15,
}

/// Synthetic device number for procfs (major=0, minor=1 — won't collide with real devs).
const PROC_DEV: u16 = 0x0001;

/// Next synthetic inode number.
static mut PROC_INO_NEXT: u32 = 100;

fn alloc_ino() -> u32 {
    unsafe {
        let n = PROC_INO_NEXT;
        PROC_INO_NEXT += 1;
        n
    }
}

/// Get a free inode slot and configure it as a proc inode.
/// Returns the inode table index, or NIL on failure.
unsafe fn proc_iget(kind: ProcKind, pid: u32, is_dir: bool) -> usize {
    unsafe {
        let idx = inode::get_empty_inode();
        if idx == NIL {
            return NIL;
        }
        let ip = inode::inode_ptr(idx);
        (*ip).i_dev = PROC_DEV;
        (*ip).i_ino = alloc_ino();
        (*ip).i_op = FsType::Proc;
        (*ip).i_count = 1;
        (*ip).i_nlink = 1;
        (*ip).i_uid = 0;
        (*ip).i_gid = 0;
        (*ip).i_size = 0;
        (*ip).i_blksize = 1024;
        (*ip).data[0] = kind as u32;
        (*ip).data[1] = (pid & 0xFFFF) as u32;
        (*ip).data[2] = ((pid >> 16) & 0xFFFF) as u32;
        if is_dir {
            (*ip).i_mode = mode::S_IFDIR | 0o555;
            (*ip).i_nlink = 2;
        } else if kind == ProcKind::SelfLink || kind == ProcKind::PidExe {
            (*ip).i_mode = mode::S_IFLNK | 0o777;
        } else {
            (*ip).i_mode = mode::S_IFREG | 0o444;
        }
        idx
    }
}

#[inline]
fn inode_kind(n: usize) -> ProcKind {
    // SAFETY: inode index is valid, data[0] was set by proc_iget
    let k = unsafe { inode::inode(n).data[0] } as u16;
    unsafe { core::mem::transmute(k) }
}

#[inline]
fn inode_pid(n: usize) -> u32 {
    // SAFETY: inode index valid
    let lo = unsafe { inode::inode(n).data[1] } as u32;
    let hi = unsafe { inode::inode(n).data[2] } as u32;
    lo | (hi << 16)
}

// ---- Superblock / mount ----

/// The inode index of /proc root (set by mount_proc).
static mut PROC_ROOT: usize = NIL;

/// Mount procfs. Called from sys_mount when fstype is "proc".
/// `dir_inode` is the mount point (must be a directory).
///
/// # Safety
/// Process context only.
pub unsafe fn mount_proc(dir_inode: usize) -> i64 {
    unsafe {
        let root = proc_iget(ProcKind::Root, 0, true);
        if root == NIL {
            return -(ENOMEM as i64);
        }
        PROC_ROOT = root;
        (*inode::inode_ptr(dir_inode)).i_mount = root;
        0
    }
}

// ---- Lookup ----

/// Lookup a name in a proc directory.
pub unsafe fn lookup(dir: usize, name: &[u8]) -> Result<usize, i32> {
    unsafe {
        let kind = inode_kind(dir);
        match kind {
            ProcKind::Root => lookup_root(name),
            ProcKind::PidDir => lookup_pid_dir(dir, name),
            _ => Err(ENOTDIR as i32),
        }
    }
}

unsafe fn lookup_root(name: &[u8]) -> Result<usize, i32> {
    unsafe {
        match name {
            b"self" => {
                let pid = crate::sched::current_index() as u32;
                let idx = proc_iget(ProcKind::SelfLink, pid, false);
                if idx == NIL { return Err(ENOMEM as i32); }
                Ok(idx)
            }
            b"version" => {
                let idx = proc_iget(ProcKind::Version, 0, false);
                if idx == NIL { return Err(ENOMEM as i32); }
                Ok(idx)
            }
            b"meminfo" => {
                let idx = proc_iget(ProcKind::Meminfo, 0, false);
                if idx == NIL { return Err(ENOMEM as i32); }
                Ok(idx)
            }
            b"stat" => {
                let idx = proc_iget(ProcKind::Stat, 0, false);
                if idx == NIL { return Err(ENOMEM as i32); }
                Ok(idx)
            }
            b"filesystems" => {
                let idx = proc_iget(ProcKind::Filesystems, 0, false);
                if idx == NIL { return Err(ENOMEM as i32); }
                Ok(idx)
            }
            _ => {
                // Try to parse as a pid number
                if let Some(pid) = parse_u32(name) {
                    if pid > 0 && (pid as usize) < crate::sched::NR_TASKS {
                        let idx = proc_iget(ProcKind::PidDir, pid, true);
                        if idx == NIL { return Err(ENOMEM as i32); }
                        return Ok(idx);
                    }
                }
                Err(ENOENT as i32)
            }
        }
    }
}

unsafe fn lookup_pid_dir(dir: usize, name: &[u8]) -> Result<usize, i32> {
    unsafe {
        let pid = inode_pid(dir);
        let (kind, is_dir) = match name {
            b"exe" => (ProcKind::PidExe, false),
            b"maps" => (ProcKind::PidMaps, false),
            b"status" => (ProcKind::PidStatus, false),
            b"cmdline" => (ProcKind::PidCmdline, false),
            b"fd" => (ProcKind::PidFdDir, true),
            _ => return Err(ENOENT as i32),
        };
        let idx = proc_iget(kind, pid, is_dir);
        if idx == NIL { return Err(ENOMEM as i32); }
        Ok(idx)
    }
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() || s.len() > 10 {
        return None;
    }
    let mut val: u32 = 0;
    for &b in s {
        if b < b'0' || b > b'9' {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(val)
}

// ---- Read ----

/// Read from a proc file. Returns bytes read or negative errno.
pub unsafe fn read(n: usize, pos: u64, buf: &mut [u8]) -> i64 {
    let kind = inode_kind(n);
    let pid = inode_pid(n);

    let mut tmp = [0u8; 512];
    let content_len = match kind {
        ProcKind::Version => fill_version(&mut tmp),
        ProcKind::Meminfo => fill_meminfo(&mut tmp),
        ProcKind::Stat => fill_stat(&mut tmp),
        ProcKind::Filesystems => fill_filesystems(&mut tmp),
        ProcKind::PidMaps => fill_pid_maps(&mut tmp, pid),
        ProcKind::PidStatus => fill_pid_status(&mut tmp, pid),
        ProcKind::PidCmdline => fill_pid_cmdline(&mut tmp, pid),
        ProcKind::SelfLink | ProcKind::PidExe => fill_pid_exe(&mut tmp, pid),
        _ => return -(EISDIR as i64),
    };

    if pos >= content_len as u64 {
        return 0;
    }
    let start = pos as usize;
    let avail = content_len - start;
    let count = avail.min(buf.len());
    buf[..count].copy_from_slice(&tmp[start..start + count]);
    count as i64
}

/// Readlink for proc symlinks.
pub unsafe fn readlink(n: usize, buf: &mut [u8]) -> i64 {
    let kind = inode_kind(n);
    let pid = inode_pid(n);
    let mut tmp = [0u8; 128];
    let len = match kind {
        ProcKind::SelfLink => {
            let s = fmt_u32(&mut tmp, pid);
            s.len()
        }
        ProcKind::PidExe => fill_pid_exe(&mut tmp, pid),
        _ => return -(EINVAL as i64),
    };
    let count = len.min(buf.len());
    buf[..count].copy_from_slice(&tmp[..count]);
    count as i64
}

// ---- Content generators ----

fn fill_version(buf: &mut [u8]) -> usize {
    let s = b"Linux version 1.0.9-shitix (rust) #1 SMP\n";
    let n = s.len().min(buf.len());
    buf[..n].copy_from_slice(&s[..n]);
    n
}

fn fill_meminfo(buf: &mut [u8]) -> usize {
    let free_kb = crate::mm::page_alloc::nr_free_pages() * 4;
    let total_kb = free_kb + 32768; // approximate: free + used estimate
    let mut w = BufWriter::new(buf);
    w.str(b"MemTotal:       ");
    w.u32(total_kb as u32);
    w.str(b" kB\nMemFree:        ");
    w.u32(free_kb as u32);
    w.str(b" kB\nMemAvailable:   ");
    w.u32(free_kb as u32);
    w.str(b" kB\nBuffers:        0 kB\nCached:         ");
    w.u32((total_kb - free_kb) as u32 / 4);
    w.str(b" kB\nSwapTotal:      0 kB\nSwapFree:       0 kB\n");
    w.pos
}

fn fill_stat(buf: &mut [u8]) -> usize {
    let s = b"cpu  0 0 0 0 0 0 0 0 0 0\ncpu0 0 0 0 0 0 0 0 0 0 0\n";
    let n = s.len().min(buf.len());
    buf[..n].copy_from_slice(&s[..n]);
    n
}

fn fill_filesystems(buf: &mut [u8]) -> usize {
    let s = b"\text4\n\tproc\n\ttmpfs\nnodev\tproc\nnodev\ttmpfs\n";
    let n = s.len().min(buf.len());
    buf[..n].copy_from_slice(&s[..n]);
    n
}

fn fill_pid_maps(_buf: &mut [u8], _pid: u32) -> usize {
    // Stub — proper VMA tracking (Task #4) would populate this
    0
}

fn fill_pid_status(buf: &mut [u8], pid: u32) -> usize {
    let mut w = BufWriter::new(buf);
    w.str(b"Name:\tprocess\nPid:\t");
    w.u32(pid);
    w.str(b"\nPPid:\t0\nState:\tR (running)\nThreads:\t1\n");
    w.pos
}

fn fill_pid_cmdline(buf: &mut [u8], pid: u32) -> usize {
    unsafe {
        let idx = pid as usize;
        if idx >= crate::sched::NR_TASKS {
            return 0;
        }
        let t = crate::sched::task_ptr(idx);
        let exe = &(*t).exe_path;
        let len = exe.iter().position(|&b| b == 0).unwrap_or(exe.len());
        let n = len.min(buf.len());
        buf[..n].copy_from_slice(&exe[..n]);
        n
    }
}

fn fill_pid_exe(buf: &mut [u8], pid: u32) -> usize {
    fill_pid_cmdline(buf, pid)
}

// ---- Directory listing ----

/// Fill a dirent for a proc directory.
pub unsafe fn fill_dirent(n: usize, pos: u64, out: &mut Dirent) -> i64 {
    let kind = inode_kind(n);
    match kind {
        ProcKind::Root => fill_root_dirent(pos, out),
        ProcKind::PidDir => fill_pid_dirent(n, pos, out),
        _ => -(ENOTDIR as i64),
    }
}

/// Root directory entries: ".", "..", "self", "version", "meminfo", "stat", "filesystems", then pid dirs
fn fill_root_dirent(pos: u64, out: &mut Dirent) -> i64 {
    const FIXED: &[&[u8]] = &[b".", b"..", b"self", b"version", b"meminfo", b"stat", b"filesystems"];
    let idx = pos as usize;

    if idx < FIXED.len() {
        let name = FIXED[idx];
        out.d_ino = (idx + 1) as u64;
        out.d_off = (idx + 1) as i64;
        out.d_reclen = core::mem::size_of::<Dirent>() as u16;
        out.d_type = if idx <= 1 { 4 } else if idx == 2 { 10 } else { 8 }; // DT_DIR=4, DT_LNK=10, DT_REG=8
        out.d_name = [0; 32];
        let n = name.len().min(31);
        out.d_name[..n].copy_from_slice(&name[..n]);
        return (idx + 1) as i64;
    }

    // After fixed entries, enumerate running tasks as pid directories
    let task_start = idx - FIXED.len();
    let mut count = 0usize;
    for i in 1..crate::sched::NR_TASKS {
        let state = unsafe { (*crate::sched::task_ptr(i)).state };
        if state == crate::sched::TaskState::Unused {
            continue;
        }
        if count == task_start {
            let mut name_buf = [0u8; 32];
            let name_len = fmt_u32_into(&mut name_buf, i as u32);
            out.d_ino = (1000 + i) as u64;
            out.d_off = (idx + 1) as i64;
            out.d_reclen = core::mem::size_of::<Dirent>() as u16;
            out.d_type = 4; // DT_DIR
            out.d_name = [0; 32];
            out.d_name[..name_len].copy_from_slice(&name_buf[..name_len]);
            return (idx + 1) as i64;
        }
        count += 1;
    }
    0 // end of directory
}

fn fill_pid_dirent(dir: usize, pos: u64, out: &mut Dirent) -> i64 {
    const ENTRIES: &[&[u8]] = &[b".", b"..", b"exe", b"maps", b"status", b"cmdline", b"fd"];
    let idx = pos as usize;
    if idx >= ENTRIES.len() {
        return 0;
    }
    let name = ENTRIES[idx];
    out.d_ino = (inode_pid(dir) * 256 + idx as u32) as u64;
    out.d_off = (idx + 1) as i64;
    out.d_reclen = core::mem::size_of::<Dirent>() as u16;
    out.d_type = if idx <= 1 { 4 } else if name == b"exe" { 10 } else if name == b"fd" { 4 } else { 8 };
    out.d_name = [0; 32];
    let n = name.len().min(31);
    out.d_name[..n].copy_from_slice(&name[..n]);
    (idx + 1) as i64
}

// ---- Helpers ----

fn fmt_u32(buf: &mut [u8], val: u32) -> &[u8] {
    let len = fmt_u32_into(buf, val);
    &buf[..len]
}

fn fmt_u32_into(buf: &mut [u8], val: u32) -> usize {
    if val == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 10];
    let mut n = 0;
    let mut v = val;
    while v > 0 {
        tmp[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    for i in 0..n {
        buf[i] = tmp[n - 1 - i];
    }
    n
}

/// Tiny buffer writer for generating proc content without alloc.
struct BufWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> BufWriter<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn str(&mut self, s: &[u8]) {
        let n = s.len().min(self.buf.len() - self.pos);
        self.buf[self.pos..self.pos + n].copy_from_slice(&s[..n]);
        self.pos += n;
    }
    fn u32(&mut self, val: u32) {
        let mut tmp = [0u8; 10];
        let len = fmt_u32_into(&mut tmp, val);
        self.str(&tmp[..len]);
    }
}
