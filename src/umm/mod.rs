//! Userspace Memory Management (UMM)
//! 
//! 为用户进程提供虚拟内存管理，包括：
//! - 用户空间地址映射
//! - 页面分配和回收
//! - 内存保护
//! - COW (Copy-On-Write) 支持

/// 用户空间起始地址 (3GB)
pub const USERSPACE_START: u64 = 0x4000_0000;

/// 用户空间结束地址 (4GB)
pub const USERSPACE_END: u64 = 0xFFFF_FFFF;

/// 用户空间大小 (3GB - 1)
pub const USERSPACE_SIZE: u64 = USERSPACE_END - USERSPACE_START;

/// 默认用户栈大小 (8MB)
pub const DEFAULT_STACK_SIZE: u64 = 8 * 1024 * 1024;

/// 默认堆起始位置 (相对于用户空间起始)
pub const DEFAULT_HEAP_START: u64 = 0x1000_0000;

/// 用户内存区域类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UmRegionType {
    None,
    Code,      // .text 段
    Data,      // .data 段  
    Bss,       // .bss 段 (未初始化数据)
    Heap,      // 动态分配区域
    Stack,     // 栈区域
    Mmap,      // mmap 映射区域
    Vvar,      // 内核映射到用户空间 (vvar)
    Vsyscall,  // vsyscall 区域
    Vsdo,      // vdso 区域
}

/// 用户内存区域权限
#[derive(Debug, Clone, Copy, Default)]
pub struct UmVmaFlags {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    pub shared: bool,
    pub private: bool,
    pub growable: bool,
    pub locked: bool,
    pub dontneed: bool,
}

impl UmVmaFlags {
    /// 创建只读权限
    pub fn read_only() -> Self {
        Self {
            read: true,
            write: false,
            exec: false,
            shared: false,
            private: true,
            growable: false,
            locked: false,
            dontneed: false,
        }
    }
    
    /// 创建读写权限
    pub fn read_write() -> Self {
        Self {
            read: true,
            write: true,
            exec: false,
            shared: false,
            private: true,
            growable: false,
            locked: false,
            dontneed: false,
        }
    }
    
    /// 创建读写执行权限
    pub fn read_write_exec() -> Self {
        Self {
            read: true,
            write: true,
            exec: true,
            shared: false,
            private: true,
            growable: false,
            locked: false,
            dontneed: false,
        }
    }
    
    /// 检查是否是可写的
    pub fn is_writable(&self) -> bool { self.write }
}

/// 用户虚拟内存区域 (VMA)
#[derive(Debug, Clone)]
pub struct UmVma {
    pub start: u64,          // 起始地址
    pub end: u64,            // 结束地址
    pub region_type: UmRegionType,
    pub flags: UmVmaFlags,
    pub file_offset: u64,    // 文件映射偏移
    pub file_path: Option<&'static str>,  // 文件路径
    pub mmap_flags: u32,    // mmap 标志
}

impl UmVma {
    /// 创建新的 VMA
    pub fn new(start: u64, end: u64, region_type: UmRegionType, flags: UmVmaFlags) -> Self {
        Self {
            start,
            end,
            region_type,
            flags,
            file_offset: 0,
            file_path: None,
            mmap_flags: 0,
        }
    }
    
    /// 获取区域大小
    pub fn size(&self) -> u64 {
        self.end - self.start
    }
    
    /// 检查地址是否在此区域内
    pub fn contains_addr(&self, addr: u64) -> bool {
        addr >= self.start && addr < self.end
    }
    
    /// 与另一个 VMA 是否重叠
    pub fn overlaps(&self, other: &UmVma) -> bool {
        self.start < other.end && other.start < self.end
    }
    
    /// 合并相邻的 VMA
    pub fn can_merge_with(&self, other: &UmVma) -> bool {
        if self.region_type != other.region_type {
            return false;
        }
        if self.file_path != other.file_path {
            return false;
        }
        // 检查标志是否相同
        self.flags.read == other.flags.read
            && self.flags.write == other.flags.write
            && self.flags.exec == other.flags.exec
            && self.flags.shared == other.flags.shared
    }
}

/// 进程用户内存结构
#[derive(Debug)]
pub struct ProcessUm {
    /// 虚拟内存区域列表
    pub vmas: [Option<UmVma>; 64],
    pub vma_count: usize,
    /// 栈基址
    pub stack_base: u64,
    /// 栈大小
    pub stack_size: u64,
    /// 堆基址
    pub heap_base: u64,
    /// 堆当前结束位置
    pub heap_brk: u64,
    /// 代码段基址
    pub code_base: u64,
    /// 代码段大小
    pub code_size: u64,
}

impl ProcessUm {
    /// 创建新的进程内存结构
    pub fn new() -> Self {
        Self {
            vmas: [const { None }; 64],
            vma_count: 0,
            stack_base: USERSPACE_END - DEFAULT_STACK_SIZE,
            stack_size: DEFAULT_STACK_SIZE,
            heap_base: DEFAULT_HEAP_START,
            heap_brk: DEFAULT_HEAP_START,
            code_base: USERSPACE_START,
            code_size: 0,
        }
    }
    
    /// 添加 VMA
    pub fn add_vma(&mut self, vma: UmVma) -> bool {
        if self.vma_count >= self.vmas.len() {
            return false;
        }
        
        // 检查是否与现有 VMA 重叠
        for i in 0..self.vma_count {
            if let Some(ref existing) = self.vmas[i] {
                if existing.overlaps(&vma) {
                    return false;
                }
            }
        }
        
        self.vmas[self.vma_count] = Some(vma);
        self.vma_count += 1;
        true
    }
    
    /// 查找包含地址的 VMA
    pub fn find_vma(&self, addr: u64) -> Option<&UmVma> {
        for i in 0..self.vma_count {
            if let Some(ref vma) = self.vmas[i] {
                if vma.contains_addr(addr) {
                    return Some(vma);
                }
            }
        }
        None
    }
    
    /// 查找包含地址的可变 VMA
    pub fn find_vma_mut(&mut self, addr: u64) -> Option<usize> {
        for i in 0..self.vma_count {
            if let Some(ref vma) = self.vmas[i] {
                if vma.contains_addr(addr) {
                    return Some(i);
                }
            }
        }
        None
    }
    
    /// 通过索引获取可变 VMA 引用
    pub fn get_vma_mut(&mut self, idx: usize) -> Option<&mut UmVma> {
        if idx >= self.vma_count {
            return None;
        }
        unsafe {
            let ptr = self.vmas.as_mut_ptr() as *mut Option<UmVma>;
            if let Some(ref mut vma) = *ptr.add(idx) {
                return Some(vma);
            }
        }
        None
    }
    
    /// 获取空闲区域
    pub fn find_free_region(&self, size: u64, align: u64, start_hint: u64) -> Option<u64> {
        let mut candidate = start_hint;
        
        // 对齐候选地址
        candidate = (candidate + align - 1) & !(align - 1);
        
        loop {
            // 检查是否在用户空间内
            if candidate + size > USERSPACE_END {
                break;
            }
            
            // 检查是否与现有 VMA 重叠
            let mut overlaps = false;
            for i in 0..self.vma_count {
                if let Some(ref vma) = self.vmas[i] {
                    if candidate < vma.end && candidate + size > vma.start {
                        overlaps = true;
                        candidate = vma.end;
                        break;
                    }
                }
            }
            
            if !overlaps {
                return Some(candidate);
            }
        }
        
        None
    }
    
    /// 删除 VMA
    pub fn remove_vma(&mut self, addr: u64) -> bool {
        for i in 0..self.vma_count {
            if let Some(ref vma) = self.vmas[i] {
                if vma.contains_addr(addr) {
                    // 移动后面的 VMA
                    for j in i..self.vma_count - 1 {
                        self.vmas[j] = self.vmas[j + 1].take();
                    }
                    self.vma_count -= 1;
                    return true;
                }
            }
        }
        false
    }
    
    /// 获取 brk 地址
    pub fn get_brk(&self) -> u64 {
        self.heap_brk
    }
    
    /// 设置 brk 地址
    pub fn set_brk(&mut self, new_brk: u64) -> bool {
        // 检查是否与栈重叠
        if new_brk > self.stack_base {
            return false;
        }
        
        // 检查是否与现有 VMA 重叠
        if let Some(ref vma) = self.find_vma(new_brk) {
            if vma.region_type != UmRegionType::Heap {
                return false;
            }
        }
        
        self.heap_brk = new_brk;
        true
    }
}

/// MMAP 标志
pub const MMAP_FIXED: u32 = 0x01;
pub const MMAP_SHARED: u32 = 0x02;
pub const MMAP_PRIVATE: u32 = 0x04;
pub const MMAP_ANONYMOUS: u32 = 0x20;
pub const MMAP_DENYWRITE: u32 = 0x1000;
pub const MMAP_EXECUTABLE: u32 = 0x2000;
pub const MMAP_LOCKED: u32 = 0x4000;
pub const MMAP_STACK: u32 = 0x20000;
pub const MMAP_HUGETLB: u32 = 0x40000;
pub const MMAP_NORESERVE: u32 = 0x4000;
pub const MMAP_GROWSDOWN: u32 = 0x0100;
pub const MMAP_HUGE_2MB: u32 = 0x5F000;
pub const MMAP_HUGE_1GB: u32 = 0x78000000;

/// MPROTECT 标志
pub const PROT_NONE: i32 = 0x0;
pub const PROT_READ: i32 = 0x1;
pub const PROT_WRITE: i32 = 0x2;
pub const PROT_EXEC: i32 = 0x4;
pub const PROT_SEM: i32 = 0x8;
pub const PROT_GROWSDOWN: i32 = 0x01000000;
pub const PROT_GROWSUP: i32 = 0x02000000;

/// 内存映射信息
#[derive(Debug, Clone, Copy)]
pub struct MmapInfo {
    pub addr: u64,
    pub len: usize,
    pub prot: i32,
    pub flags: u32,
    pub fd: i32,
    pub offset: i64,
}

impl MmapInfo {
    /// 创建匿名映射
    pub fn anonymous(addr: u64, len: usize, prot: i32) -> Self {
        Self {
            addr,
            len,
            prot,
            flags: MMAP_ANONYMOUS | MMAP_PRIVATE,
            fd: -1,
            offset: 0,
        }
    }
}

/// 用户内存统计
#[derive(Debug, Clone, Default)]
pub struct UmStat {
    pub total_size: u64,
    pub code_size: u64,
    pub data_size: u64,
    pub heap_size: u64,
    pub stack_size: u64,
    pub mmap_size: u64,
    pub vma_count: usize,
}

impl ProcessUm {
    /// 获取内存统计
    pub fn get_stat(&self) -> UmStat {
        let mut stat = UmStat::default();
        stat.vma_count = self.vma_count;
        
        for i in 0..self.vma_count {
            if let Some(ref vma) = self.vmas[i] {
                let size = vma.size();
                stat.total_size += size;
                
                match vma.region_type {
                    UmRegionType::Code => stat.code_size += size,
                    UmRegionType::Data | UmRegionType::Bss => stat.data_size += size,
                    UmRegionType::Heap => stat.heap_size += size,
                    UmRegionType::Stack => stat.stack_size += size,
                    UmRegionType::Mmap => stat.mmap_size += size,
                    _ => {}
                }
            }
        }
        
        stat
    }
}

/// ELF 段加载信息
#[derive(Debug, Clone)]
pub struct ElfSegment {
    pub vaddr: u64,
    pub memsz: u64,
    pub filesz: u64,
    pub offset: u64,
    pub flags: u32,
}

impl ElfSegment {
    /// 获取权限标志
    pub fn get_vma_flags(&self) -> UmVmaFlags {
        UmVmaFlags {
            read: (self.flags & 0x04) != 0,
            write: (self.flags & 0x02) != 0,
            exec: (self.flags & 0x01) != 0,
            shared: false,
            private: true,
            growable: false,
            locked: false,
            dontneed: false,
        }
    }
    
    /// 获取区域类型
    pub fn get_region_type(&self) -> UmRegionType {
        if self.filesz == 0 && self.memsz > 0 {
            UmRegionType::Bss
        } else {
            UmRegionType::Data
        }
    }
}

/// 用户空间内存管理初始化
pub fn umm_init() {
    crate::pr_info!("UMM: Initializing userspace memory manager...");
    crate::pr_info!("UMM: User space: 0x{:08x} - 0x{:08x} ({:.1}GB)",
        USERSPACE_START, USERSPACE_END, USERSPACE_SIZE as f64 / (1024.0 * 1024.0 * 1024.0));
    crate::pr_info!("UMM: Default heap: 0x{:08x}", DEFAULT_HEAP_START);
}

/// 为进程创建初始内存布局
pub fn create_initial_layout() -> ProcessUm {
    let mut um = ProcessUm::new();
    
    // 创建代码段 VMA
    um.add_vma(UmVma::new(
        USERSPACE_START,
        USERSPACE_START + 0x1000,
        UmRegionType::Code,
        UmVmaFlags::read_only(),
    ));
    
    // 创建堆段 VMA
    um.add_vma(UmVma::new(
        DEFAULT_HEAP_START,
        DEFAULT_HEAP_START + 0x1000,
        UmRegionType::Heap,
        UmVmaFlags::read_write(),
    ));
    
    // 创建栈段 VMA
    um.add_vma(UmVma::new(
        um.stack_base,
        USERSPACE_END,
        UmRegionType::Stack,
        UmVmaFlags::read_write(),
    ));
    
    um
}

/// 检查地址是否是用户空间地址
pub fn is_user_addr(addr: u64) -> bool {
    addr >= USERSPACE_START && addr < USERSPACE_END
}

/// 检查地址是否可读
pub fn can_read(addr: u64, um: &ProcessUm) -> bool {
    if let Some(vma) = um.find_vma(addr) {
        vma.flags.read
    } else {
        false
    }
}

/// 检查地址是否可写
pub fn can_write(addr: u64, um: &ProcessUm) -> bool {
    if let Some(vma) = um.find_vma(addr) {
        vma.flags.write
    } else {
        false
    }
}

/// 检查地址是否可执行
pub fn can_exec(addr: u64, um: &ProcessUm) -> bool {
    if let Some(vma) = um.find_vma(addr) {
        vma.flags.exec
    } else {
        false
    }
}

/// 打印 VMA 列表
pub fn print_vmas(um: &ProcessUm) {
    crate::pr_info!("UMM: VMA list (count={}):", um.vma_count);
    for i in 0..um.vma_count {
        if let Some(ref vma) = um.vmas[i] {
            let type_str = match vma.region_type {
                UmRegionType::Code => "code",
                UmRegionType::Data => "data",
                UmRegionType::Bss => "bss",
                UmRegionType::Heap => "heap",
                UmRegionType::Stack => "stack",
                UmRegionType::Mmap => "mmap",
                _ => "unknown",
            };
            let r = if vma.flags.read { 'r' } else { '-' };
            let w = if vma.flags.write { 'w' } else { '-' };
            let x = if vma.flags.exec { 'x' } else { '-' };
            crate::pr_info!("  [{:2}] 0x{:08x}-0x{:08x} {:6} {}{}{}",
                i, vma.start, vma.end, type_str, r, w, x);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vma_contains() {
        let vma = UmVma::new(0x40000000, 0x40001000, UmRegionType::Code, UmVmaFlags::read_only());
        assert!(vma.contains_addr(0x40000000));
        assert!(vma.contains_addr(0x40000FFF));
        assert!(!vma.contains_addr(0x40001000));
        assert!(!vma.contains_addr(0x3FFFFFFF));
    }

    #[test]
    fn test_vma_overlap() {
        let vma1 = UmVma::new(0x40000000, 0x40001000, UmRegionType::Code, UmVmaFlags::read_only());
        let vma2 = UmVma::new(0x40000800, 0x40002000, UmRegionType::Code, UmVmaFlags::read_only());
        assert!(vma1.overlaps(&vma2));
    }

    #[test]
    fn test_process_um() {
        let mut um = ProcessUm::new();
        assert!(um.add_vma(UmVma::new(0x40000000, 0x40001000, UmRegionType::Code, UmVmaFlags::read_only())));
        assert!(um.find_vma(0x40000000).is_some());
        assert!(um.find_vma(0x50000000).is_none());
    }
}
