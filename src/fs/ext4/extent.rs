//! ext4 extent (范围) 数据结构
//! 
//! ext4 使用 extent 树代替 ext2 的间接块映射来存储文件数据块的地址。
//! 这大大提高了大文件的性能，减少了元数据开销。
//! 
//! ## 术语
//! 
//! - **extent**: 表示一系列连续的物理块。一个 extent 由起始块和长度描述。
//! - **extent tree**: B 树结构，用于存储和管理文件的 extent。

/// extent 头结构
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct ExtentHeader {
    /// magic (0xF30A)
    pub eh_magic: u16,
    /// 条目数量
    pub eh_entries: u16,
    /// 分配的条目最大数量
    pub eh_max: u16,
    /// 深度 (0 = leaf)
    pub eh_depth: u16,
    /// 世代数 (用于一致性检查)
    pub eh_generation: u32,
}

impl ExtentHeader {
    /// extent 头 magic
    pub const MAGIC: u16 = 0xF30A;
    
    /// 从字节创建
    pub unsafe fn from_bytes(data: &[u8; 12]) -> Self {
        Self {
            eh_magic: u16::from_le_bytes([data[0], data[1]]),
            eh_entries: u16::from_le_bytes([data[2], data[3]]),
            eh_max: u16::from_le_bytes([data[4], data[5]]),
            eh_depth: u16::from_le_bytes([data[6], data[7]]),
            eh_generation: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
        }
    }
    
    /// 验证 magic
    pub fn is_valid(&self) -> bool {
        self.eh_magic == Self::MAGIC
    }
    
    /// 是否是叶子节点
    pub fn is_leaf(&self) -> bool {
        self.eh_depth == 0
    }
    
    /// 获取条目数
    pub fn entries(&self) -> usize {
        self.eh_entries as usize
    }
    
    /// 获取深度
    pub fn depth(&self) -> usize {
        self.eh_depth as usize
    }
}

/// extent 索引条目 (用于内部节点)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct ExtentIdx {
    /// 指向子节点的块号
    pub ei_leaf_lo: u32,
    /// 起始逻辑块号
    pub ei_start: u32,
    /// 结束逻辑块号
    pub ei_end: u32,
    /// 指向实际块的高位和标志
    pub ei_leaf_hi: u16,
    /// 未使用
    pub ei_unused: u16,
}

impl ExtentIdx {
    /// 从字节创建
    pub unsafe fn from_bytes(data: &[u8; 12]) -> Self {
        Self {
            ei_leaf_lo: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            ei_start: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            ei_end: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            ei_leaf_hi: 0,
            ei_unused: 0,
        }
    }
    
    /// 获取叶子节点块号 (64 位)
    pub fn leaf_block(&self) -> u64 {
        (self.ei_leaf_hi as u64) << 32 | (self.ei_leaf_lo as u64)
    }
}

/// extent 条目 (用于叶子节点)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Extent {
    /// 起始物理块号 (低位)
    pub ee_block_lo: u32,
    /// 起始块号 (高位) 和长度 (低位)
    pub ee_len_hi_start_hi: u16,
    /// 长度 (高位)
    pub ee_len_hi: u16,
    /// 起始块号 (最高位) 和长度 (最高位)
    pub ee_start_hi: u16,
    /// 长度 (最高位)
    pub ee_len_max: u16,
}

impl Extent {
    /// 从字节创建
    pub unsafe fn from_bytes(data: &[u8; 12]) -> Self {
        Self {
            ee_block_lo: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            ee_len_hi_start_hi: u16::from_le_bytes([data[4], data[5]]),
            ee_len_hi: u16::from_le_bytes([data[6], data[7]]),
            ee_start_hi: u16::from_le_bytes([data[8], data[9]]),
            ee_len_max: u16::from_le_bytes([data[10], data[11]]),
        }
    }
    
    /// 获取起始逻辑块号
    pub fn ee_block(&self) -> u32 {
        self.ee_block_lo
    }
    
    /// 获取物理块号 (64 位)
    pub fn ee_start(&self) -> u64 {
        let high = (self.ee_start_hi as u64) << 32;
        let mid = (self.ee_len_hi_start_hi as u64 & 0x003F) << 32;
        let low = self.ee_block_lo as u64;
        high | mid | low
    }
    
    /// 获取长度
    pub fn ee_len(&self) -> u32 {
        let max16 = (self.ee_len_max as u32) << 16;
        let mid16 = (self.ee_len_hi as u32) << 8;
        let lo = self.ee_len_hi_start_hi as u32;
        ((max16 | mid16 | lo) & 0x3FFF) as u32
    }
    
    /// 获取结束逻辑块号 (不包含)
    pub fn ee_end(&self) -> u32 {
        self.ee_block() + self.ee_len() - 1
    }
    
    /// 检查是否是未写入的 extent
    pub fn is_unwritten(&self) -> bool {
        (self.ee_len_hi_start_hi & 0x8000) != 0
    }
}

/// extent 树结构
/// 
/// 用于管理文件的 extent 树
pub struct ExtentTree {
    /// 指向 extent 头的指针
    header: ExtentHeader,
    /// 条目数组 (最大 4 个)
    entries: [u8; 48],
}

impl ExtentTree {
    /// 获取叶子 extent 条目
    pub fn get_leaf_extents(&self) -> &[Extent] {
        // 这是简化版本，实际需要从 inode 数据中解析
        unsafe {
            core::slice::from_raw_parts(
                self.entries.as_ptr() as *const Extent,
                4
            )
        }
    }
    
    /// 获取索引条目
    pub fn get_index_entries(&self) -> &[ExtentIdx] {
        unsafe {
            core::slice::from_raw_parts(
                self.entries.as_ptr() as *const ExtentIdx,
                4
            )
        }
    }
}

/// 在 extent 中查找逻辑块对应的物理块
/// 
/// # Arguments
/// * `extents` - extent 数组指针
/// * `num_extents` - extent 数量
/// * `lblock` - 逻辑块号
/// 
/// # Returns
/// * `Some(physical_block)` - 物理块号
/// * `None` - 块不在任何 extent 中 (可能是空洞)
pub fn ext4_ext_find_goal(extents: *const Extent, num_extents: usize, lblock: u32) -> Option<u64> {
    if extents.is_null() || num_extents == 0 {
        return None;
    }
    
    let exts = unsafe { core::slice::from_raw_parts(extents, num_extents) };
    
    for ext in exts.iter() {
        let start = ext.ee_block();
        let len = ext.ee_len();
        let end = start + len;
        
        if lblock >= start && lblock < end {
            let offset = lblock - start;
            return Some(ext.ee_start() + offset as u64);
        }
    }
    
    None
}

/// 计算 extent 条目能表示的最大长度
pub const EXT_INIT_MAX_LEN: u32 = 0x8000;  // 2^15
pub const EXT_UNWRITTEN_MAX_LEN: u32 = 0x8000;  // 2^15

/// 最大的 extent 长度
pub const EXT4_MAX_EXTENT_LEN: u32 = 32768;  // 32KB = 32 * 1024 blocks
