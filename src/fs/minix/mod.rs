//! minix 文件系统。对应 linux-1.0.9 的 `fs/minix/`。
//!
//! # 磁盘布局（同原版 `include/linux/minix_fs.h`）
//!
//! | 块号 | 内容 |
//! |---|---|
//! | 0 | 引导块（不用）|
//! | 1 | 超级块 |
//! | 2 .. 2+imap_blocks | inode 位图 |
//! | .. +zmap_blocks | zone 位图 |
//! | .. | inode 表（每块 32 个 `minix_inode`）|
//! | firstdatazone .. nzones | 数据区 |
//!
//! # 移植范围
//!
//! | 本模块 | 原版 |
//! |---|---|
//! | [`bitmap`] | `bitmap.c`：`new_block`/`free_block`/`new_inode`/`free_inode` |
//! | [`inode_ops`] | `inode.c`：`read_super`/`read_inode`/`write_inode`/`bmap`/`getblk` |
//! | [`namei`] | `namei.c`：`lookup`/`create`/`mkdir`/`unlink`/`rmdir`/`link` |
//! | [`dir`] | `dir.c`：`readdir` |
//! | [`file`] | `file.c`：`file_read`/`file_write` |
//! | [`truncate`] | `truncate.c`：`truncate` |
//! | [`mkfs`] | 原版没有 —— 见下 |
//!
//! [`mkfs`] 是我们加的：原版的根文件系统映像是外部造好、由 `rd_load()`
//! 从软驱读进 ramdisk 的。我们没有软驱也没有外部映像，所以在内存里
//! 直接铺一个空的 minix v1 文件系统出来。它写的是**磁盘格式**，
//! 因此同时也是对 [`inode_ops`] 那些读取代码的独立交叉验证：
//! 布局理解错了两边不会同时错成一样。
//!
//! 不移植：`symlink.c`（需要 `follow_link` 与 `readlink`，而我们的
//! [`namei`] 还没有符号链接展开循环）、`fsync.c`（`minix_sync_file`
//! 的逐块回写；`fsync_dev` 已经覆盖了整设备回写这个更粗的粒度）、
//! minix V2（原版自己也只是留了 `new_minix_inode` 结构体没实现）。

pub mod bitmap;
pub mod dir;
pub mod file;
pub mod inode_ops;
pub mod mkfs;
pub mod namei;
pub mod truncate;

pub use bitmap::{free_block, free_inode, new_block, new_inode};
pub use inode_ops::{
    bmap, get_block, minix_bread, put_inode, put_super, read_inode, read_super, write_inode,
    write_super,
};
pub use mkfs::mkfs;

/// 根 inode 号。对应原版 `MINIX_ROOT_INO 1`。
pub const MINIX_ROOT_INO: u32 = 1;

/// 最大链接数。对应原版 `MINIX_LINK_MAX 250`。
pub const MINIX_LINK_MAX: u16 = 250;

/// 原始 minix。对应原版 `MINIX_SUPER_MAGIC 0x137F`（名字最长 14）。
pub const MINIX_SUPER_MAGIC: u32 = 0x137F;
/// 30 字符名字的变体。对应原版 `MINIX_SUPER_MAGIC2 0x138F`。
pub const MINIX_SUPER_MAGIC2: u32 = 0x138F;

/// 文件系统干净。对应原版 `MINIX_VALID_FS 0x0001`。
pub const MINIX_VALID_FS: u16 = 0x0001;
/// 文件系统有错。对应原版 `MINIX_ERROR_FS 0x0002`。
pub const MINIX_ERROR_FS: u16 = 0x0002;

/// 磁盘 inode 的大小。对应原版 `sizeof(struct minix_inode)`，
/// 原版在 `minix_read_super` 里 `if (32 != sizeof(...)) panic("bad i-node size")`。
pub const MINIX_INODE_SIZE: usize = 32;

/// 每块几个 inode。对应原版 `MINIX_INODES_PER_BLOCK`。
pub const MINIX_INODES_PER_BLOCK: u32 = (crate::fs::BLOCK_SIZE / MINIX_INODE_SIZE) as u32;

/// 一块能装几个 zone 号（间接块的扇出）。原版里写成字面量 512
/// （1024 字节 / 2 字节）。
pub const ZONES_PER_BLOCK: u32 = (crate::fs::BLOCK_SIZE / 2) as u32;

/// 直接块个数。原版 `i_zone[0..7]`，第 7 项是一级间接、第 8 项是二级间接。
pub const DIRECT_ZONES: usize = 7;
/// 一级间接在 `i_zone` 里的下标。
pub const IND_ZONE: usize = 7;
/// 二级间接的下标。
pub const DIND_ZONE: usize = 8;

/// 位图一块能标多少位。原版里写成 8192（1024 字节 × 8）。
pub const BITS_PER_BLOCK: u32 = (crate::fs::BLOCK_SIZE * 8) as u32;

/// 位图槽位数。原版 `MINIX_I_MAP_SLOTS`/`MINIX_Z_MAP_SLOTS` 都是 8，
/// 两个位图共用这个上限，所以包成一个函数供 `bitmap` 用。
#[inline]
pub const fn super_block_slots() -> usize {
    crate::fs::super_block::MINIX_I_MAP_SLOTS
}

/// 磁盘上的 inode。对应原版 `struct minix_inode`。
///
/// 布局必须与磁盘字节序完全一致（小端，i386 原生），所以是 `#[repr(C)]`
/// 加显式的 `u16`/`u32`。原版的 `unsigned long` 在 i386 上是 32 位，
/// 这里写成 `u32`（x86_64 上 `long` 是 64 位，直译会错 4 个字节 ——
/// 这是移植 32 位内核磁盘结构体时最容易踩的一处）。
#[derive(Clone, Copy)]
#[repr(C)]
pub struct MinixInode {
    /// 类型与权限。原版 `unsigned short i_mode`
    pub i_mode: u16,
    /// 属主。原版 `unsigned short i_uid`
    pub i_uid: u16,
    /// 大小。原版 `unsigned long i_size`（i386 的 long = 32 位）
    pub i_size: u32,
    /// 时间戳（只有一个，v1 没分 atime/mtime/ctime）。原版 `unsigned long i_time`
    pub i_time: u32,
    /// 属组。原版 `unsigned char i_gid`
    pub i_gid: u8,
    /// 链接数。原版 `unsigned char i_nlinks`
    pub i_nlinks: u8,
    /// 数据块号：0-6 直接、7 一级间接、8 二级间接。原版 `unsigned short i_zone[9]`
    pub i_zone: [u16; 9],
}

impl MinixInode {
    /// 全零的 inode。
    pub const fn zeroed() -> Self {
        MinixInode {
            i_mode: 0,
            i_uid: 0,
            i_size: 0,
            i_time: 0,
            i_gid: 0,
            i_nlinks: 0,
            i_zone: [0; 9],
        }
    }
}

/// 磁盘上的超级块。对应原版 `struct minix_super_block`。
///
/// 同 [`MinixInode`]：`s_max_size` 原版是 `unsigned long`，i386 上 32 位。
#[derive(Clone, Copy)]
#[repr(C)]
pub struct MinixSuperBlock {
    pub s_ninodes: u16,
    pub s_nzones: u16,
    pub s_imap_blocks: u16,
    pub s_zmap_blocks: u16,
    pub s_firstdatazone: u16,
    pub s_log_zone_size: u16,
    pub s_max_size: u32,
    pub s_magic: u16,
    pub s_state: u16,
}

impl MinixSuperBlock {
    pub const fn zeroed() -> Self {
        MinixSuperBlock {
            s_ninodes: 0,
            s_nzones: 0,
            s_imap_blocks: 0,
            s_zmap_blocks: 0,
            s_firstdatazone: 0,
            s_log_zone_size: 0,
            s_max_size: 0,
            s_magic: 0,
            s_state: 0,
        }
    }
}

/// 磁盘上的目录项。对应原版 `struct minix_dir_entry`：
/// `{ unsigned short inode; char name[0]; }`——名字是变长的尾巴，
/// 实际长度由超级块的 `s_dirsize` 决定（16 或 32 字节一项）。
///
/// Rust 里没有 `char name[0]` 这种写法，所以这里只定义头部，
/// 名字通过 `s_dirsize` 手工切片（见 [`dir`] 与 [`namei`]）。
pub const DIRENT_INO_SIZE: usize = 2;

/// 编译期检查磁盘结构体大小。原版在运行时 panic
/// （`minix_read_super` 里那句 `if (32 != sizeof (struct minix_inode))`），
/// Rust 能在编译期做掉。
const _: () = assert!(core::mem::size_of::<MinixInode>() == MINIX_INODE_SIZE);
const _: () = assert!(core::mem::size_of::<MinixSuperBlock>() == 20);
