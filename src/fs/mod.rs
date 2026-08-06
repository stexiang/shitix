//! 虚拟文件系统层。对应 linux-1.0.9 的 `fs/` 目录与 `include/linux/fs.h`。
//!
//! # 与原版的结构性差异
//!
//! 1. **指针链 → 下标链**。原版所有缓存（buffer_head / inode / file / super_block）
//!    都是裸指针双向链表。我们沿用模块 4 里 `WaitQueue` 的做法：定长静态数组 +
//!    `u16`/`usize` 下标，`NIL` 表示空。这样 `static mut` 里不出现自引用裸指针，
//!    SAFETY 论证只需要「下标在界内」而不是「指针非悬垂」。
//! 2. **`i_op`/`f_op` 函数指针表 → `FsType` 枚举 + 静态分发**。原版
//!    `struct inode_operations` 是 15 个函数指针。目前只有 minix 一个磁盘文件系统
//!    加上字符/块设备，用枚举分发既不丢语义又省掉一堆 `Option<fn>` 判空。
//!    真正需要多文件系统并存时再改回 vtable。
//! 3. **`union u`（各文件系统私有 inode 数据）→ 具名字段 `data: [u16; 16]`**。
//!    minix 的 `minix_inode_info` 就是这个数组；其他文件系统没移植。
//! 4. 没有 `i_sem`/`i_mmap`/`i_socket`/`i_flock`：分别属于尚未移植的
//!    信号量、`mm/mmap.c`、网络、`fs/locks.c`。

pub mod buffer;
pub mod devices;
pub mod ext2;        // ext2 filesystem support
pub mod ext4;        // ext4 filesystem support
pub mod file_table;
pub mod inode;
pub mod minix;
pub mod namei;
pub mod open;
pub mod read_write;
pub mod stat;
pub mod super_block;

pub use buffer::{BLOCK_SIZE, bread, brelse, getblk, sync_dev};
pub use devices::{block_read, block_write, chrdev_read, chrdev_write};
pub use ext2::{
    Ext2SuperBlock,     // ext2 superblock
    Ext2Inode,          // ext2 inode structure
    Ext2GroupDesc,       // ext2 block group descriptor
};
pub use ext4::{
    Ext4SuperBlock,      // ext4 superblock
    Ext4Inode,           // ext4 inode structure
    Ext4FeatureFlags,    // ext4 feature flags
};
pub use file_table::{File, get_empty_filp};
pub use inode::{Inode, iget, iput};
pub use super_block::{SuperBlock, mount_root};

/// 原版 `NR_OPEN 256`。每进程 fd 上限；我们的 `Task` 目前只装 16 个。
pub const NR_OPEN: usize = 16;
/// 原版 `NR_INODE 2048`，缩到 64。
pub const NR_INODE: usize = 64;
/// 原版 `NR_FILE 1024`，缩到 32。
pub const NR_FILE: usize = 32;
/// 原版 `NR_SUPER 32`，缩到 4。
pub const NR_SUPER: usize = 4;

/// 访问权限位，对应原版 `MAY_EXEC`/`MAY_WRITE`/`MAY_READ`。
pub const MAY_EXEC: u16 = 1;
pub const MAY_WRITE: u16 = 2;
pub const MAY_READ: u16 = 4;

/// `ll_rw_block` 的命令。数值同原版 `READ`/`WRITE`/`READA`/`WRITEA`。
pub const READ: i32 = 0;
pub const WRITE: i32 = 1;
pub const READA: i32 = 2;
pub const WRITEA: i32 = 3;

/// 挂载标志，对应原版 `MS_*`。
pub const MS_RDONLY: u64 = 1;
pub const MS_NOSUID: u64 = 2;
pub const MS_NODEV: u64 = 4;
pub const MS_NOEXEC: u64 = 8;
pub const MS_SYNC: u64 = 16;
pub const MS_REMOUNT: u64 = 32;

/// 设备号打包/拆包。对应原版 `MAJOR`/`MINOR`/`MKDEV` 三个宏。
/// 原版 `dev_t` 是 16 位：高 8 位主设备号，低 8 位次设备号。
#[inline]
pub const fn major(dev: u16) -> u32 {
    (dev >> 8) as u32
}

#[inline]
pub const fn minor(dev: u16) -> u32 {
    (dev & 0xFF) as u32
}

#[inline]
pub const fn mkdev(ma: u32, mi: u32) -> u16 {
    (((ma & 0xFF) << 8) | (mi & 0xFF)) as u16
}

/// 文件类型与权限位。对应原版 `include/linux/stat.h` 的 `S_*`。
pub mod mode {
    pub const S_IFMT: u16 = 0o170000;
    pub const S_IFLNK: u16 = 0o120000;
    pub const S_IFREG: u16 = 0o100000;
    pub const S_IFBLK: u16 = 0o060000;
    pub const S_IFDIR: u16 = 0o040000;
    pub const S_IFCHR: u16 = 0o020000;
    pub const S_IFIFO: u16 = 0o010000;
    pub const S_ISUID: u16 = 0o004000;
    pub const S_ISGID: u16 = 0o002000;
    pub const S_ISVTX: u16 = 0o001000;

    pub const S_IRWXU: u16 = 0o0700;
    pub const S_IRUSR: u16 = 0o0400;
    pub const S_IWUSR: u16 = 0o0200;
    pub const S_IXUSR: u16 = 0o0100;
    pub const S_IRWXG: u16 = 0o0070;
    pub const S_IRWXO: u16 = 0o0007;

    #[inline]
    pub const fn is_lnk(m: u16) -> bool {
        m & S_IFMT == S_IFLNK
    }
    #[inline]
    pub const fn is_reg(m: u16) -> bool {
        m & S_IFMT == S_IFREG
    }
    #[inline]
    pub const fn is_dir(m: u16) -> bool {
        m & S_IFMT == S_IFDIR
    }
    #[inline]
    pub const fn is_chr(m: u16) -> bool {
        m & S_IFMT == S_IFCHR
    }
    #[inline]
    pub const fn is_blk(m: u16) -> bool {
        m & S_IFMT == S_IFBLK
    }
    #[inline]
    pub const fn is_fifo(m: u16) -> bool {
        m & S_IFMT == S_IFIFO
    }
}

/// 打开标志。对应原版 `include/linux/fcntl.h` 的 `O_*`。
pub mod oflags {
    pub const O_ACCMODE: u32 = 0o003;
    pub const O_RDONLY: u32 = 0o0;
    pub const O_WRONLY: u32 = 0o1;
    pub const O_RDWR: u32 = 0o2;
    pub const O_CREAT: u32 = 0o100;
    pub const O_EXCL: u32 = 0o200;
    pub const O_NOCTTY: u32 = 0o400;
    pub const O_TRUNC: u32 = 0o1000;
    pub const O_APPEND: u32 = 0o2000;
    pub const O_NONBLOCK: u32 = 0o4000;
}

/// `lseek` 的 whence。对应原版 `SEEK_SET`/`SEEK_CUR`/`SEEK_END`。
pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;

/// 目录项，返回给 `getdents` 一类的调用。对应原版 `include/linux/dirent.h`。
#[repr(C)]
pub struct Dirent {
    pub d_ino: u64,
    pub d_off: i64,
    pub d_reclen: u16,
    /// 原版是 `char d_name[NAME_MAX+1]`，NAME_MAX=255；minix 名字最长 30，取 32。
    pub d_name: [u8; 32],
}

/// 初始化整个 VFS：buffer cache → inode 表 → file 表 → 设备表。
/// 对应原版 `start_kernel` 里那串 `buffer_init(); inode_init(); file_table_init();`。
///
/// # Safety
/// 启动期调用一次，此时不能有其他任务在跑 fs 代码。
pub unsafe fn init() {
    // SAFETY: 契约转交。顺序同原版 start_kernel：缓冲缓存最先
    // （inode 与超级块都要通过它读盘），设备表要在驱动 init 之前建好。
    unsafe {
        buffer::init();
        inode::init();
        file_table::init();
        super_block::init();
        open::init();
        devices::init();
    }
}
