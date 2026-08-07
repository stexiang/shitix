//! Userspace Memory Management (UMM)
//! 
//! 为用户进程提供虚拟内存管理，包括：
//! - 用户空间地址映射
//! - 页面分配和回收
//! - 内存保护
//! - COW (Copy-On-Write) 支持

/// 用户空间起始地址 (3GB)
pub const USERSPACE_START: u64 = 0x4000_0000;

/// COW 页面标记 - 在 VMA flags 中使用
pub const VMA_COW: u32 = 0x10000000;

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

/// COW (Copy-On-Write) 支持

/// 检查 VMA 是否应该使用 COW
pub fn vma_is_cow(vma: &UmVma) -> bool {
    vma.flags.private && vma.flags.read && !vma.flags.write
}

/// 设置 VMA 为 COW 模式
pub fn vma_set_cow(vma: &mut UmVma, cow: bool) {
    if cow {
        vma.mmap_flags |= VMA_COW;
        // COW 页面应该是只读的，直到发生写入
        vma.flags.write = false;
    } else {
        vma.mmap_flags &= !VMA_COW;
    }
}

/// 为 COW fork 复制用户内存
/// 
/// 复制源进程的 VMA 到目标进程，标记为 COW
/// 返回复制的 VMA 数量
pub fn cow_fork_copy_vmas(src: &ProcessUm, dst: &mut ProcessUm) -> usize {
    let mut count = 0;
    
    for i in 0..src.vma_count {
        if let Some(ref vma) = src.vmas[i] {
            // 跳过内核空间
            if vma.start >= USERSPACE_START {
                let mut new_vma = vma.clone();
                
                // 私有页面在 fork 后标记为 COW
                if vma.flags.private && !vma.flags.write {
                    new_vma.mmap_flags |= VMA_COW;
                }
                
                if dst.add_vma(new_vma) {
                    count += 1;
                }
            }
        }
    }
    
    count
}

/// 处理 COW 页面的页面错误
/// 
/// 当进程尝试写入一个 COW 页面时调用此函数
/// 如果页面只被当前进程使用，直接启用写权限
/// 如果页面被多个进程共享，分配新页面并复制内容
/// 
/// # Arguments
/// * `fault_addr` - 触发错误的虚拟地址
/// * `pml4` - 当前进程的页表根地址
/// 
/// # Returns
/// * `true` - 成功处理
/// * `false` - 处理失败
pub fn handle_cow_page_fault(fault_addr: u64, pml4: usize) -> bool {
    use crate::mm::paging;
    use crate::mm::page_ref;
    
    // 首先检查页表项
    let pte = unsafe { crate::mm::paging::translate(pml4, fault_addr as usize) };
    if pte.is_none() {
        crate::pr_warn!("COW: page fault at {:x} - no mapping", fault_addr);
        return false;
    }
    
    let phys = pte.unwrap();
    let pfn = page_ref::phys_to_pfn(phys);
    
    // 增加引用计数，因为我们正在创建新的映射
    page_ref::page_ref_inc(pfn);
    
    // 更新页表，启用写权限
    // 移除 COW 标记，设置读写权限
    unsafe {
        // 重新映射为读写
        let _ = crate::mm::paging::unmap_page(pml4, fault_addr as usize);
        let prot = crate::mm::paging::flags::SHARED; // present + rw + user
        crate::mm::paging::map_page(pml4, fault_addr as usize, phys, prot);
    }
    
    // 取消 COW 标记
    page_ref::unmark_page_cow(pfn);
    
    crate::pr_debug!("COW: page fault resolved at {:x}", fault_addr);
    true
}

/// 在 fork 时设置 COW 页面
/// 
/// 将页表项设置为只读并标记为 COW
pub fn cow_setup_page(pml4: usize, vaddr: usize, phys: usize) {
    use crate::mm::page_ref;
    
    let pfn = page_ref::phys_to_pfn(phys);
    
    // 增加引用计数
    page_ref::page_ref_inc(pfn);
    
    // 设置为只读 (COW)
    unsafe {
        let _ = crate::mm::paging::unmap_page(pml4, vaddr);
        crate::mm::paging::map_page(pml4, vaddr, phys, crate::mm::paging::flags::READONLY);
    }
    
    // 标记为 COW 页面
    page_ref::mark_page_cow(pfn);
}

/// 释放进程的 COW 页面引用
pub fn cow_release_vmas(um: &ProcessUm, pml4: usize) {
    use crate::mm::page_ref;
    
    for i in 0..um.vma_count {
        if let Some(ref vma) = um.vmas[i] {
            if vma.start < USERSPACE_START {
                continue;
            }
            
            // 遍历页面并减少引用计数
            let mut vaddr = vma.start;
            while vaddr < vma.end {
                if let Some(phys) = unsafe { crate::mm::paging::translate(pml4, vaddr as usize) } {
                    let pfn = page_ref::phys_to_pfn(phys);
                    let refs = page_ref::page_ref_dec(pfn);
                    
                    // 如果引用计数降到 0，释放页面
                    if refs == 0 {
                        unsafe {
                            let _ = crate::mm::paging::unmap_page(pml4, vaddr as usize);
                        }
                        // 页面会被 page allocator 回收
                    }
                }
                vaddr += 4096;
            }
        }
    }
}

// =============================================================================
// Stage 3: 用户进程创建
// =============================================================================

/// 用户进程的页表与地址空间布局。
pub struct UserSpace {
    pub pml4: usize,
    pub code_start: u64,
    pub code_size: usize,
    pub stack_top: u64,
    pub stack_size: usize,
    /// 进程本身的虚存管理器（VMA 表，等 execve 后才真正使用）。
    pub um: ProcessUm,
}

/// 创建一个最小的用户态进程。
///
/// 分配 PML4、代码页、栈页，设置页表映射，把 `code` 拷贝进代码页。
/// 返回 `UserSpace` 结构（物理 PML4 地址、虚地址、大小等）。
///
/// 代码段始于 `USERSPACE_START`（0x4000_0000），栈底在下一个 4KB 页。
pub fn create_user_process(code: &[u8]) -> Result<UserSpace, i32> {
    use crate::mm::get_free_page;
    use crate::klib::errno::ENOMEM;

    let code_size = code.len();
    if code_size > crate::mm::PAGE_SIZE {
        crate::pr_warn!("create_user_process: code too large ({} > {})", code_size, crate::mm::PAGE_SIZE);
        return Err(-(ENOMEM as i32));
    }

    // 1. 分配 PML4
    let pml4 = crate::mm::paging::alloc_pml4();
    if pml4 == 0 {
        crate::pr_warn!("create_user_process: OOM for PML4");
        return Err(-(ENOMEM as i32));
    }
    // SAFETY: 刚分配的页，在恒等映射内。
    unsafe { core::ptr::write_bytes(pml4 as *mut u8, 0, crate::mm::PAGE_SIZE) }

    // 2. 共享内核 PDPT[0]，这样内核代码仍然可访问
    if !crate::mm::paging::clone_kernel_pdpt(pml4) {
        crate::mm::free_page(pml4);
        crate::pr_warn!("create_user_process: clone_kernel_pdpt failed");
        return Err(-(ENOMEM as i32));
    }

    // 3. 分配代码页与栈页
    let code_page = get_free_page();
    let stack_page = get_free_page();
    if code_page == 0 || stack_page == 0 {
        if code_page != 0 { crate::mm::free_page(code_page); }
        if stack_page != 0 { crate::mm::free_page(stack_page); }
        crate::mm::free_page(pml4);
        crate::pr_warn!("create_user_process: OOM for code/stack pages");
        return Err(-(ENOMEM as i32));
    }

    let code_start: u64 = USERSPACE_START;   // 0x4000_0000
    let stack_top: u64 = USERSPACE_START + crate::mm::PAGE_SIZE as u64 * 2; // 0x4000_2000
    let stack_vaddr: u64 = USERSPACE_START + crate::mm::PAGE_SIZE as u64;    // 0x4000_1000

    // 4. 映射代码页（可读可执行，暂时 rwx；等 NX 落地后去掉 exec 对数据页）
    // SAFETY: 页表、虚地址、物理地址都有效。
    unsafe {
        if !crate::mm::paging::map_page(pml4, code_start as usize, code_page, crate::mm::paging::flags::SHARED) {
            crate::mm::free_page(code_page);
            crate::mm::free_page(stack_page);
            crate::mm::free_page(pml4);
            return Err(-(ENOMEM as i32));
        }
        // 映射栈页
        if !crate::mm::paging::map_page(pml4, stack_vaddr as usize, stack_page, crate::mm::paging::flags::SHARED) {
            crate::mm::paging::unmap_page(pml4, code_start as usize);
            crate::mm::free_page(code_page);
            crate::mm::free_page(stack_page);
            crate::mm::free_page(pml4);
            return Err(-(ENOMEM as i32));
        }
    }

    // 5. 拷贝代码到代码页（恒等映射下 phys == virt）
    // SAFETY: code_page 在恒等映射低 1GB 内，我们独占它。
    unsafe {
        core::ptr::copy_nonoverlapping(code.as_ptr(), code_page as *mut u8, code_size);
    }

    // 6. 验证：通过用户页表能 translate 且 USER 位全路径有效。
    {
        let t = unsafe { crate::mm::paging::translate(pml4, code_start as usize) };
        if t != Some(code_page) {
            crate::pr_warn!("create_user_process: translate failed");
            crate::mm::free_page(code_page);
            crate::mm::free_page(stack_page);
            crate::mm::free_page(pml4);
            return Err(-(ENOMEM as i32));
        }
    }

    let mut um = ProcessUm::new();
    um.add_vma(UmVma::new(
        code_start,
        code_start + crate::mm::PAGE_SIZE as u64,
        UmRegionType::Code,
        UmVmaFlags::read_only(), // COW fork 时会变成只读
    ));
    um.add_vma(UmVma::new(
        stack_vaddr,
        stack_top,
        UmRegionType::Stack,
        UmVmaFlags::read_write(),
    ));

    Ok(UserSpace {
        pml4,
        code_start,
        code_size,
        stack_top,
        stack_size: crate::mm::PAGE_SIZE,
        um,
    })
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

/// 尝试处理 COW 页面故障
/// 
/// 从 page fault handler 调用，检查是否是 COW 页面故障并进行相应处理。
/// 
/// # Arguments
/// * `fault_addr` - 故障地址
/// * `pml4` - 当前进程的页表根
/// 
/// # Returns
/// * `Some(true)` - 成功处理了 COW 故障
/// * `Some(false)` - 不是 COW 故障，需要其他处理
/// * `None` - 错误发生
pub unsafe fn try_handle_cow_fault(fault_addr: u64, pml4: usize) -> Option<bool> {
    use crate::mm::page_ref;
    
    // 确保地址在用户空间
    if fault_addr < USERSPACE_START as u64 || fault_addr >= USERSPACE_END as u64 {
        return Some(false);
    }
    
    // 检查页面是否存在且是只读
    // SAFETY: 调用者确保 pml4 有效，fault_addr 是用户空间地址
    if let Some(phys) = unsafe { crate::mm::paging::translate(pml4, fault_addr as usize) } {
        let pfn = page_ref::phys_to_pfn(phys);
        
        // 检查页面引用计数
        let refs = page_ref::page_ref_count(pfn);
        
        // 如果引用计数 > 1，说明是共享页面 (COW)
        if refs > 1 {
            // 需要复制页面
            let new_page = crate::mm::get_free_page();
            if new_page == 0 {
                crate::pr_warn!("COW: out of memory");
                return None;
            }
            
            // 复制页面内容
            let src = phys as *const u8;
            let dst = new_page as *mut u8;
            // SAFETY: 两边都是有效的物理页面
            unsafe {
                core::ptr::copy_nonoverlapping(src, dst, crate::mm::page::PAGE_SIZE);
            }
            
            // 更新页表
            // SAFETY: 映射新的物理页面
            let _ = unsafe { crate::mm::paging::unmap_page(pml4, fault_addr as usize) };
            let prot = crate::mm::paging::flags::SHARED; // present + rw + user
            if !unsafe { crate::mm::paging::map_page(pml4, fault_addr as usize, new_page, prot) } {
                crate::pr_warn!("COW: failed to map new page");
                return None;
            }
            
            // 减少原页面的引用计数
            page_ref::page_ref_dec(pfn);
            
            // 释放新页面的引用计数（因为它现在是唯一引用）
            let new_pfn = page_ref::phys_to_pfn(new_page);
            page_ref::page_ref_set(new_pfn, 1);
            
            crate::pr_debug!("COW: copied page from {:x} to {:x}", phys, new_page);
            return Some(true);
        } else if refs == 1 {
            // 引用计数为 1，只需要启用写权限
            let flags = crate::mm::paging::get_page_flags(pml4, fault_addr as usize);
            if let Some(flags) = flags {
                // 设置写权限
                if !crate::mm::paging::set_page_flags(pml4, fault_addr as usize, 
                    flags | crate::mm::paging::flags::RW) {
                    crate::pr_warn!("COW: failed to set write flags");
                    return None;
                }
            }
            crate::pr_debug!("COW: enabled write for single-reference page");
            return Some(true);
        }
    }
    
    // 不是 COW 故障
    Some(false)
}
