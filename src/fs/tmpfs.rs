//! Minimal tmpfs: RAM-backed filesystem for /tmp, /run, etc.
//!
//! Stores file data in statically allocated pages. Directory entries are stored
//! in a flat table. Sufficient for LFS boot scripts that write to /tmp.

use crate::fs::inode::{self, FsType, Inode, NIL};
use crate::fs::mode;
use crate::fs::Dirent;
use crate::klib::errno::*;

const TMPFS_DEV: u16 = 0x0002;
const MAX_FILES: usize = 16;
const MAX_DATA_PAGES: usize = 16;
const PAGE_SIZE: usize = 4096;

/// A tmpfs directory entry.
#[derive(Clone, Copy)]
struct TmpEntry {
    name: [u8; 32],
    name_len: u8,
    inode_idx: usize,
    parent: usize, // inode index of parent dir
}

/// A data page for file content.
static mut DATA_PAGES: [[u8; PAGE_SIZE]; MAX_DATA_PAGES] = [[0; PAGE_SIZE]; MAX_DATA_PAGES];
static mut DATA_PAGE_USED: [bool; MAX_DATA_PAGES] = [false; MAX_DATA_PAGES];

/// Directory entry table.
static mut ENTRIES: [TmpEntry; MAX_FILES] = [TmpEntry {
    name: [0; 32],
    name_len: 0,
    inode_idx: NIL,
    parent: NIL,
}; MAX_FILES];
static mut ENTRY_COUNT: usize = 0;

/// Map inode index → first data page index. data[0..2] stores start page, data[2..4] stores page count.
static mut INO_NEXT: u32 = 200;

fn alloc_ino() -> u32 {
    unsafe {
        let n = INO_NEXT;
        INO_NEXT += 1;
        n
    }
}

fn alloc_data_page() -> Option<usize> {
    unsafe {
        for i in 0..MAX_DATA_PAGES {
            if !DATA_PAGE_USED[i] {
                DATA_PAGE_USED[i] = true;
                DATA_PAGES[i] = [0; PAGE_SIZE];
                return Some(i);
            }
        }
        None
    }
}

/// Create a tmpfs inode.
unsafe fn tmpfs_iget(is_dir: bool, perm: u16) -> usize {
    unsafe {
        let idx = inode::get_empty_inode();
        if idx == NIL {
            return NIL;
        }
        let ip = inode::inode_ptr(idx);
        (*ip).i_dev = TMPFS_DEV;
        (*ip).i_ino = alloc_ino();
        (*ip).i_op = FsType::Tmpfs;
        (*ip).i_count = 1;
        (*ip).i_nlink = 1;
        (*ip).i_uid = 0;
        (*ip).i_gid = 0;
        (*ip).i_size = 0;
        (*ip).i_blksize = PAGE_SIZE as u32;
        (*ip).data = [0; 9];
        if is_dir {
            (*ip).i_mode = mode::S_IFDIR | perm;
            (*ip).i_nlink = 2;
        } else {
            (*ip).i_mode = mode::S_IFREG | perm;
        }
        idx
    }
}

static mut TMPFS_ROOT: usize = NIL;

/// Mount tmpfs at the given directory inode.
pub unsafe fn mount_tmpfs(dir_inode: usize) -> i64 {
    unsafe {
        let root = tmpfs_iget(true, 0o1777);
        if root == NIL {
            return -(ENOMEM as i64);
        }
        TMPFS_ROOT = root;
        (*inode::inode_ptr(dir_inode)).i_mount = root;
        0
    }
}

// ---- Lookup ----

pub unsafe fn lookup(dir: usize, name: &[u8]) -> Result<usize, i32> {
    unsafe {
        if name == b"." {
            inode::inode_ptr(dir).as_mut().unwrap().i_count += 1;
            return Ok(dir);
        }
        if name == b".." {
            // Find parent
            for i in 0..ENTRY_COUNT {
                if ENTRIES[i].inode_idx == dir {
                    let p = ENTRIES[i].parent;
                    if p != NIL {
                        inode::inode_ptr(p).as_mut().unwrap().i_count += 1;
                        return Ok(p);
                    }
                }
            }
            inode::inode_ptr(dir).as_mut().unwrap().i_count += 1;
            return Ok(dir);
        }
        for i in 0..ENTRY_COUNT {
            if ENTRIES[i].parent == dir
                && ENTRIES[i].name_len == name.len() as u8
                && &ENTRIES[i].name[..name.len()] == name
            {
                let idx = ENTRIES[i].inode_idx;
                inode::inode_ptr(idx).as_mut().unwrap().i_count += 1;
                return Ok(idx);
            }
        }
        Err(ENOENT as i32)
    }
}

// ---- Create ----

/// Create a file in a tmpfs directory (used by open with O_CREAT).
pub unsafe fn create(dir: usize, name: &[u8], mode_bits: u16) -> Result<usize, i32> {
    unsafe {
        if name.len() > 31 || ENTRY_COUNT >= MAX_FILES {
            return Err(ENOSPC as i32);
        }
        let is_dir = (mode_bits & mode::S_IFMT) == mode::S_IFDIR;
        let idx = tmpfs_iget(is_dir, mode_bits & 0o7777);
        if idx == NIL {
            return Err(ENOMEM as i32);
        }
        let e = &mut ENTRIES[ENTRY_COUNT];
        e.name = [0; 32];
        e.name[..name.len()].copy_from_slice(name);
        e.name_len = name.len() as u8;
        e.inode_idx = idx;
        e.parent = dir;
        ENTRY_COUNT += 1;
        Ok(idx)
    }
}

// ---- Read / Write ----

/// Read from a tmpfs file. Data stored in data pages indexed by inode data[0..2].
pub unsafe fn read(n: usize, pos: u64, buf: &mut [u8]) -> i64 {
    unsafe {
        let ip = inode::inode_ptr(n);
        let size = (*ip).i_size as u64;
        if pos >= size {
            return 0;
        }
        let avail = (size - pos) as usize;
        let count = avail.min(buf.len());

        let start_page_idx = (*ip).data[0] as usize;
        let mut offset = pos as usize;
        let mut written = 0;

        while written < count {
            let page_local = offset / PAGE_SIZE;
            let page_off = offset % PAGE_SIZE;
            let page_idx = start_page_idx + page_local;
            if page_idx >= MAX_DATA_PAGES || !DATA_PAGE_USED[page_idx] {
                break;
            }
            let chunk = (PAGE_SIZE - page_off).min(count - written);
            buf[written..written + chunk]
                .copy_from_slice(&DATA_PAGES[page_idx][page_off..page_off + chunk]);
            written += chunk;
            offset += chunk;
        }
        written as i64
    }
}

/// Write to a tmpfs file.
pub unsafe fn write(n: usize, pos: u64, buf: &[u8]) -> i64 {
    unsafe {
        let ip = inode::inode_ptr(n);
        let m = (*ip).i_mode;
        if mode::is_dir(m) {
            return -(EISDIR as i64);
        }

        // Allocate pages if this is the first write
        let mut start_page = (*ip).data[0] as usize;
        let mut page_count = (*ip).data[1] as usize;

        if page_count == 0 {
            // Allocate first page
            if let Some(pg) = alloc_data_page() {
                (*ip).data[0] = pg as u16;
                (*ip).data[1] = 1;
                start_page = pg;
                page_count = 1;
            } else {
                return -(ENOSPC as i64);
            }
        }

        let end_offset = pos as usize + buf.len();
        // Allocate more pages if needed
        let pages_needed = (end_offset + PAGE_SIZE - 1) / PAGE_SIZE;
        while page_count < pages_needed {
            let next = start_page + page_count;
            if next >= MAX_DATA_PAGES {
                return -(ENOSPC as i64);
            }
            if !DATA_PAGE_USED[next] {
                DATA_PAGE_USED[next] = true;
                DATA_PAGES[next] = [0; PAGE_SIZE];
            }
            page_count += 1;
            (*ip).data[1] = page_count as u16;
        }

        let mut offset = pos as usize;
        let mut read_pos = 0;
        while read_pos < buf.len() {
            let page_local = offset / PAGE_SIZE;
            let page_off = offset % PAGE_SIZE;
            let page_idx = start_page + page_local;
            let chunk = (PAGE_SIZE - page_off).min(buf.len() - read_pos);
            DATA_PAGES[page_idx][page_off..page_off + chunk]
                .copy_from_slice(&buf[read_pos..read_pos + chunk]);
            read_pos += chunk;
            offset += chunk;
        }

        if end_offset > (*ip).i_size as usize {
            (*ip).i_size = end_offset as u32;
        }
        buf.len() as i64
    }
}

// ---- Directory listing ----

pub unsafe fn fill_dirent(n: usize, pos: u64, out: &mut Dirent) -> i64 {
    unsafe {
        let idx = pos as usize;
        // First two entries: "." and ".."
        if idx == 0 {
            out.d_ino = inode::inode(n).i_ino as u64;
            out.d_off = 1;
            out.d_reclen = core::mem::size_of::<Dirent>() as u16;
            out.d_type = 4;
            out.d_name = [0; 32];
            out.d_name[0] = b'.';
            return 1;
        }
        if idx == 1 {
            out.d_ino = inode::inode(n).i_ino as u64;
            out.d_off = 2;
            out.d_reclen = core::mem::size_of::<Dirent>() as u16;
            out.d_type = 4;
            out.d_name = [0; 32];
            out.d_name[0] = b'.';
            out.d_name[1] = b'.';
            return 2;
        }

        // Enumerate children of this directory
        let child_idx = idx - 2;
        let mut count = 0usize;
        for i in 0..ENTRY_COUNT {
            if ENTRIES[i].parent == n {
                if count == child_idx {
                    let e = &ENTRIES[i];
                    let ci = e.inode_idx;
                    out.d_ino = inode::inode(ci).i_ino as u64;
                    out.d_off = (idx + 1) as i64;
                    out.d_reclen = core::mem::size_of::<Dirent>() as u16;
                    let cm = inode::inode(ci).i_mode;
                    out.d_type = if mode::is_dir(cm) { 4 } else { 8 };
                    out.d_name = [0; 32];
                    let nl = e.name_len as usize;
                    out.d_name[..nl].copy_from_slice(&e.name[..nl]);
                    return (idx + 1) as i64;
                }
                count += 1;
            }
        }
        0 // end
    }
}
