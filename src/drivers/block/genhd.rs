//! 通用磁盘接口。参考 linux-1.0.9 的 `drivers/block/genhd.c`。
//!
//! ## 功能
//!
//! - 分区表解析
//! - 磁盘几何信息
//! - gendisk 结构管理
//! - 分区检查
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `genhd.c` | 通用磁盘驱动 |
//! | `blk.h` | 块设备头文件 |

/// 主设备号
pub const MAJOR_SHIFT: u8 = 8;

/// 主设备号常量
pub mod major {
    /// 未命名的设备
    pub const UNNAMED: u8 = 0;
    /// DAC960 RAID
    pub const DAC960: u8 = 110;
    /// DASD 磁盘
    pub const DASD: u8 = 94;
    /// XT 磁盘
    pub const XT: u8 = 13;
    /// SCSI 磁盘
    pub const SCSI_DISK: u8 = 8;
    /// SCSI CD-ROM
    pub const SCSI_CDROM: u8 = 11;
    /// 老式 MFM/IDE
    pub const MFM_DISK: u8 = 3;
    /// DAC960 辅助
    pub const DAC960_AUX: u8 = 109;
    /// 软盘
    pub const FLOPPY: u8 = 2;
    /// PS/2 ESDI
    pub const PS2ESDI: u8 = 12;
    /// 并口 IDE
    pub const BLKBLK: u8 = 37;
    /// 软盘（IBM 类型）
    pub const COMPAQ: u8 = 38;
    /// ATARI SCSI
    pub const ATARI_SCSI: u8 = 96;
    /// ATARI ACSI
    pub const ATARI_ACSI: u8 = 97;
    /// Commodore Amiga
    pub const AMIGA_ZORRAM: u8 = 98;
    /// 内存盘
    pub const RAMDISK: u8 = 1;
}

/// 磁盘分区信息
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Partition {
    /// 起始扇区
    pub start_sect: u32,
    /// 分区大小（扇区数）
    pub nr_sects: u32,
}

impl Partition {
    /// 创建新的分区
    pub const fn new() -> Self {
        Partition {
            start_sect: 0,
            nr_sects: 0,
        }
    }
    
    /// 检查分区是否有效
    pub fn is_valid(&self) -> bool {
        self.nr_sects > 0
    }
}

/// 分区表条目。参考 `struct partition`。
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PartitionTable {
    /// 引导指示符（0x80 = 可引导）
    pub boot_flag: u8,
    /// 起始磁头
    pub start_head: u8,
    /// 起始扇区（低 6 位）和柱面（高 10 位）
    pub start_sector: u8,
    /// 起始柱面
    pub start_cyl: u16,
    /// 分区类型
    pub sys_ind: u8,
    /// 结束磁头
    pub end_head: u8,
    /// 结束扇区和柱面
    pub end_sector: u8,
    /// 结束柱面
    pub end_cyl: u16,
    /// 相对起始扇区
    pub start_sect: u32,
    /// 分区大小（扇区数）
    pub nr_sects: u32,
}

impl PartitionTable {
    /// 从原始数据解析分区表
    pub fn from_raw(data: &[u8; 16], offset: usize) -> Self {
        let base = &data[offset..offset + 16];
        PartitionTable {
            boot_flag: base[0],
            start_head: base[1],
            start_sector: base[2],
            start_cyl: u16::from_le_bytes([base[3], base[4]]),
            sys_ind: base[4],
            end_head: base[5],
            end_sector: base[6],
            end_cyl: u16::from_le_bytes([base[7], base[8]]),
            start_sect: u32::from_le_bytes([base[8], base[9], base[10], base[11]]),
            nr_sects: u32::from_le_bytes([base[12], base[13], base[14], base[15]]),
        }
    }
    
    /// 检查分区表是否有效（MBR 签名）
    pub fn is_valid_mbr(data: &[u8]) -> bool {
        if data.len() < 512 {
            return false;
        }
        // MBR 签名在偏移 510 处
        u16::from_le_bytes([data[510], data[511]]) == 0xAA55
    }
    
    /// 获取分区类型名称
    pub fn type_name(&self) -> &'static str {
        match self.sys_ind {
            0x00 => "Empty",
            0x01 => "FAT12",
            0x04 => "FAT16 <32M",
            0x05 => "Extended",
            0x06 => "FAT16",
            0x07 => "HPFS/NTFS",
            0x08 => "AIX",
            0x09 => "AIX bootable",
            0x0A => "OS/2 Boot Manager",
            0x0B => "W95 FAT32",
            0x0C => "W95 FAT32 (LBA)",
            0x0E => "W95 FAT16 (LBA)",
            0x0F => "W95 Extended (LBA)",
            0x10 => "OPUS",
            0x11 => "Hidden FAT12",
            0x12 => "Compaq diagnostics",
            0x14 => "Hidden FAT16 <32M",
            0x16 => "Hidden FAT16",
            0x17 => "Hidden HPFS/NTFS",
            0x18 => "AST SmartStart",
            0x1B => "Hidden W95 FAT32",
            0x1C => "Hidden W95 FAT32 (LBA)",
            0x1E => "Hidden W95 FAT16 (LBA)",
            0x3C => "PartMagic recovery",
            0x81 => "Minix",
            0x82 => "Linux swap",
            0x83 => "Linux",
            0x85 => "Linux extended",
            0x86 => "NTFS volume set",
            0x87 => "NTFS volume set",
            0x8E => "Linux LVM",
            0xA5 => "FreeBSD",
            0xA6 => "OpenBSD",
            0xA8 => "Darwin UFS",
            0xAF => "HFS/HFS+",
            0xEB => "BeOS filesystem",
            0xEE => "EFI GPT",
            0xEF => "EFI (FAT-12/16/32)",
            0xFD => "Linux raid autodetect",
            _ => "Unknown",
        }
    }
    
    /// 检查是否是扩展分区
    pub fn is_extended(&self) -> bool {
        self.sys_ind == 0x05 || self.sys_ind == 0x0F
    }
}

/// 通用磁盘结构
pub const MAX_NR_PARTITIONS: usize = 16;

/// 磁盘设备信息
#[derive(Debug)]
pub struct GenDisk {
    /// 主设备号
    pub major: u8,
    /// 设备名
    pub name: &'static str,
    /// 次设备号位移
    pub minor_shift: u8,
    /// 最大分区数
    pub max_p: u8,
    /// 最大次设备号
    pub max_nr: u16,
    /// 裸设备名
    pub major_name: &'static str,
    /// 分区表
    pub partitions: [Partition; MAX_NR_PARTITIONS],
}

impl GenDisk {
    /// 创建新的磁盘设备
    pub fn new(major: u8, name: &'static str, minor_shift: u8, max_p: u8, max_nr: u16) -> Self {
        GenDisk {
            major,
            name,
            minor_shift,
            max_p,
            max_nr,
            major_name: name,
            partitions: [const { Partition::new() }; MAX_NR_PARTITIONS],
        }
    }
    
    /// 获取设备号
    pub fn device_num(&self, minor: u32) -> u32 {
        ((self.major as u32) << MAJOR_SHIFT) | (minor & 0xFF)
    }
    
    /// 获取分区数
    pub fn partition_count(&self) -> usize {
        self.partitions.iter().filter(|p| p.is_valid()).count()
    }
}

/// 磁盘几何信息
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct DiskGeometry {
    /// 柱面数
    pub cylinders: u16,
    /// 磁头数
    pub heads: u8,
    /// 每磁道扇区数
    pub sectors: u8,
}

impl DiskGeometry {
    /// 从 CHS 转换为扇区数
    pub fn to_sectors(&self) -> u64 {
        (self.cylinders as u64) * (self.heads as u64) * (self.sectors as u64)
    }
    
    /// 创建新的几何信息
    pub fn new(cylinders: u16, heads: u8, sectors: u8) -> Self {
        DiskGeometry {
            cylinders,
            heads,
            sectors,
        }
    }
}

/// 解析 MBR 分区表
pub fn parse_mbr(data: &[u8; 512]) -> [Option<PartitionTable>; 4] {
    let mut partitions = [None, None, None, None];
    
    if !PartitionTable::is_valid_mbr(data) {
        return partitions;
    }
    
    for i in 0..4 {
        let offset = 0x1BE + (i * 16);
        let mut part_data = [0u8; 16];
        part_data.copy_from_slice(&data[offset..offset + 16]);
        partitions[i] = Some(PartitionTable::from_raw(&part_data, 0));
    }
    
    partitions
}

/// 初始化 genhd 模块
pub fn init() {
    crate::sprintln!("genhd: generic disk interface initialized");
}

/// 运行自检
pub fn selftest() {
    crate::sprintln!("--- genhd selftest ---");
    
    // 测试 MBR 签名检测
    let mut valid_mbr = [0u8; 512];
    valid_mbr[510] = 0x55;
    valid_mbr[511] = 0xAA;
    assert!(PartitionTable::is_valid_mbr(&valid_mbr));
    
    let mut invalid_mbr = [0u8; 512];
    assert!(!PartitionTable::is_valid_mbr(&invalid_mbr));
    
    // 测试分区类型名称
    let linux_part = PartitionTable {
        boot_flag: 0,
        start_head: 0,
        start_sector: 0,
        start_cyl: 0,
        sys_ind: 0x83,
        end_head: 0,
        end_sector: 0,
        end_cyl: 0,
        start_sect: 0,
        nr_sects: 1000,
    };
    assert_eq!(linux_part.type_name(), "Linux");
    
    // 测试 GenDisk
    let disk = GenDisk::new(8, "sd", 4, 16, 64);
    assert_eq!(disk.major, 8);
    assert_eq!(disk.partition_count(), 0);
    
    // 测试 DiskGeometry
    let geo = DiskGeometry::new(1024, 255, 63);
    assert_eq!(geo.to_sectors(), 1024 * 255 * 63);
    
    crate::sprintln!("genhd: MBR parsing, partition types -> ok");
}
