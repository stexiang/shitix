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

/// extent 索引条目（内部节点），对应内核 `struct ext4_extent_idx`
///
/// 磁盘布局（12 字节，小端）：
/// ```text
/// 0..4   ei_block    该索引覆盖的起始逻辑块号
/// 4..8   ei_leaf_lo  下一级节点所在物理块号低 32 位
/// 8..10  ei_leaf_hi  下一级节点所在物理块号高 16 位
/// 10..12 ei_unused
/// ```
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct ExtentIdx {
    /// 该索引覆盖的起始逻辑块号
    pub ei_block: u32,
    /// 子节点物理块号低 32 位
    pub ei_leaf_lo: u32,
    /// 子节点物理块号高 16 位
    pub ei_leaf_hi: u16,
    /// 未使用
    pub ei_unused: u16,
}

impl ExtentIdx {
    /// 磁盘上的条目大小
    pub const SIZE: usize = 12;

    /// 从 12 字节小端数据解析
    pub fn from_bytes(data: &[u8; 12]) -> Self {
        Self {
            ei_block: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            ei_leaf_lo: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            ei_leaf_hi: u16::from_le_bytes([data[8], data[9]]),
            ei_unused: u16::from_le_bytes([data[10], data[11]]),
        }
    }

    /// 起始逻辑块号
    #[inline]
    pub fn ei_block(&self) -> u32 {
        self.ei_block
    }

    /// 子节点物理块号（48 位）
    #[inline]
    pub fn leaf_block(&self) -> u64 {
        ((self.ei_leaf_hi as u64) << 32) | (self.ei_leaf_lo as u64)
    }
}

/// extent 条目（叶子节点），对应内核 `struct ext4_extent`
///
/// 磁盘布局（12 字节，小端）：
/// ```text
/// 0..4   ee_block    起始**逻辑**块号
/// 4..6   ee_len      长度。bit15 置位表示 unwritten（预分配未写），
///                    此时真实长度 = ee_len - 32768
/// 6..8   ee_start_hi 起始**物理**块号高 16 位
/// 8..12  ee_start_lo 起始**物理**块号低 32 位
/// ```
/// 物理块号总共 48 位。已初始化 extent 的长度上限是 32768 块，
/// unwritten 的上限是 32767（因为 bit15 被借去当标志）。
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Extent {
    /// 起始逻辑块号
    pub ee_block: u32,
    /// 长度（含 unwritten 标志位）
    pub ee_len: u16,
    /// 起始物理块号高 16 位
    pub ee_start_hi: u16,
    /// 起始物理块号低 32 位
    pub ee_start_lo: u32,
}

impl Extent {
    /// 磁盘上的条目大小
    pub const SIZE: usize = 12;

    /// 从 12 字节小端数据解析
    pub fn from_bytes(data: &[u8; 12]) -> Self {
        Self {
            ee_block: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            ee_len: u16::from_le_bytes([data[4], data[5]]),
            ee_start_hi: u16::from_le_bytes([data[6], data[7]]),
            ee_start_lo: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
        }
    }

    /// 起始逻辑块号
    #[inline]
    pub fn ee_block(&self) -> u32 {
        self.ee_block
    }

    /// 起始物理块号（48 位）
    #[inline]
    pub fn ee_start(&self) -> u64 {
        ((self.ee_start_hi as u64) << 32) | (self.ee_start_lo as u64)
    }

    /// 真实长度（块数）。unwritten extent 要减掉 bit15。
    ///
    /// 对应内核 `ext4_ext_get_actual_len()`。
    #[inline]
    pub fn ee_len(&self) -> u32 {
        let raw = self.ee_len;
        if raw <= EXT_INIT_MAX_LEN as u16 {
            raw as u32
        } else {
            (raw - EXT_INIT_MAX_LEN as u16) as u32
        }
    }

    /// 最后一个逻辑块号（含）。长度为 0 时返回起始块号，不下溢。
    #[inline]
    pub fn ee_end(&self) -> u32 {
        let len = self.ee_len();
        if len == 0 { self.ee_block() } else { self.ee_block() + len - 1 }
    }

    /// 是否是 unwritten（预分配但未写）extent
    #[inline]
    pub fn is_unwritten(&self) -> bool {
        self.ee_len > EXT_INIT_MAX_LEN as u16
    }
}

/// 一个 extent 树节点（inode 的 `i_block` 60 字节，或一个磁盘块）
///
/// 原来这里是个自带 48 字节数组、`get_leaf_extents()` 硬返回 4 条的壳子，
/// 与磁盘上「头后面紧跟 eh_entries 条」的实际布局无关。改成在调用方给的
/// 缓冲上按偏移解析，条目数取自头部。
pub struct ExtentNode<'a> {
    /// 节点原始字节（含 12 字节头）
    raw: &'a [u8],
    /// 已校验的头
    header: ExtentHeader,
}

impl<'a> ExtentNode<'a> {
    /// 解析一个节点。magic 不对或条目数超出缓冲则返回 `None`。
    pub fn parse(raw: &'a [u8]) -> Option<Self> {
        if raw.len() < 12 {
            return None;
        }
        let mut hdr = [0u8; 12];
        hdr.copy_from_slice(&raw[..12]);
        // SAFETY: 长度已确认为 12。
        let header = unsafe { ExtentHeader::from_bytes(&hdr) };
        if !header.is_valid() {
            return None;
        }
        // eh_entries 必须落在缓冲内，否则是损坏的节点。
        let need = 12 + header.entries() * Extent::SIZE;
        if need > raw.len() || header.entries() > header.eh_max as usize {
            return None;
        }
        Some(Self { raw, header })
    }

    /// 节点头
    #[inline]
    pub fn header(&self) -> &ExtentHeader {
        &self.header
    }

    /// 取第 `i` 条（叶子）。越界返回 `None`。
    pub fn extent(&self, i: usize) -> Option<Extent> {
        if i >= self.header.entries() || !self.header.is_leaf() {
            return None;
        }
        let off = 12 + i * Extent::SIZE;
        let mut buf = [0u8; 12];
        buf.copy_from_slice(&self.raw[off..off + 12]);
        Some(Extent::from_bytes(&buf))
    }

    /// 取第 `i` 条（索引）。越界或本节点是叶子则返回 `None`。
    pub fn index(&self, i: usize) -> Option<ExtentIdx> {
        if i >= self.header.entries() || self.header.is_leaf() {
            return None;
        }
        let off = 12 + i * ExtentIdx::SIZE;
        let mut buf = [0u8; 12];
        buf.copy_from_slice(&self.raw[off..off + 12]);
        Some(ExtentIdx::from_bytes(&buf))
    }

    /// 在叶子节点里查逻辑块 `lblock` 对应的物理块。
    ///
    /// 返回 `None` 表示空洞（sparse file 的未分配区）。unwritten extent
    /// 也返回物理块号，调用方需要自己看 `is_unwritten()` 决定是否读盘
    /// （内核的做法是给用户返回全零而不真读）。
    pub fn lookup_leaf(&self, lblock: u32) -> Option<Extent> {
        if !self.header.is_leaf() {
            return None;
        }
        // 条目按 ee_block 升序，二分。对应内核 ext4_ext_binsearch()。
        let n = self.header.entries();
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let e = self.extent(mid)?;
            if e.ee_block() > lblock { hi = mid } else { lo = mid + 1 }
        }
        if lo == 0 {
            return None; // lblock 在第一条之前
        }
        let e = self.extent(lo - 1)?;
        if lblock <= e.ee_end() { Some(e) } else { None }
    }

    /// 在内部节点里选出应该下探的子节点物理块号。
    ///
    /// 对应内核 `ext4_ext_binsearch_idx()`。
    pub fn lookup_index(&self, lblock: u32) -> Option<u64> {
        if self.header.is_leaf() {
            return None;
        }
        let n = self.header.entries();
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let ix = self.index(mid)?;
            if ix.ei_block() > lblock { hi = mid } else { lo = mid + 1 }
        }
        if lo == 0 {
            return None;
        }
        Some(self.index(lo - 1)?.leaf_block())
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

    // SAFETY: 调用方保证 extents 指向 num_extents 个连续的 Extent。
    let exts = unsafe { core::slice::from_raw_parts(extents, num_extents) };

    for ext in exts.iter() {
        let start = ext.ee_block();
        let len = ext.ee_len();
        if len == 0 {
            continue;
        }
        // 用 u64 算上界，避免 start + len 在接近 u32::MAX 时回绕
        // （回绕后区间判断会把界外的 lblock 判成命中）。
        if (lblock as u64) >= start as u64 && (lblock as u64) < start as u64 + len as u64 {
            let offset = lblock - start;
            return Some(ext.ee_start() + offset as u64);
        }
    }

    None
}

/// 已初始化 extent 的最大长度（块数）。也是 `ee_len` 里 unwritten 的标志位。
pub const EXT_INIT_MAX_LEN: u32 = 32768; // 2^15
/// unwritten extent 的最大长度：bit15 被借去当标志，只剩 32767
pub const EXT_UNWRITTEN_MAX_LEN: u32 = 32767;
