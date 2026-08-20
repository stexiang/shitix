//! ELF (Executable and Linkable Format) 加载器
//! 
//! 提供 ELF 文件解析和加载功能，用于 execve 系统调用。

/// ELF 魔数
pub const ELF_MAGIC: u32 = 0x464C457F;

/// ELF 类别
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ElfClass {
    None = 0,
    ELF32 = 1,
    ELF64 = 2,
}

/// ELF 数据编码
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ElfData {
    None = 0,
    LittleEndian = 1,
    BigEndian = 2,
}

/// ELF 类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ElfType {
    None = 0,
    Relocatable = 1,
    Executable = 2,
    Shared = 3,
    Core = 4,
}

/// ELF 机器架构
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ElfMachine {
    None = 0,
    X86 = 3,
    X86_64 = 62,
    Arm = 40,
    RiscV = 243,
}

/// ELF 程序段类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ElfPType {
    Null = 0,
    Load = 1,
    Dynamic = 2,
    Interp = 3,
    Note = 4,
    ShLib = 5,
    Phdr = 6,
    GnuStack = 0x6474e551,
}

/// ELF 程序头
#[repr(C)]
pub struct Elf32Phdr {
    pub p_type: u32,
    pub p_offset: u32,
    pub p_vaddr: u32,
    pub p_paddr: u32,
    pub p_filesz: u32,
    pub p_memsz: u32,
    pub p_flags: u32,
    pub p_align: u32,
}

/// ELF 文件头 (32位)
#[repr(C)]
pub struct Elf32Header {
    pub e_ident: [u8; 16],
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u32,
    pub e_phoff: u32,
    pub e_shoff: u32,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

/// ELF 文件头 (64位)
#[repr(C)]
pub struct Elf64Header {
    pub e_ident: [u8; 16],
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_shoff: u64,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

/// ELF 程序头 (64位)
#[repr(C)]
pub struct Elf64Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

/// ELF 验证错误
#[derive(Debug, Clone, Copy)]
pub enum ElfError {
    InvalidMagic,
    InvalidClass,
    InvalidData,
    InvalidVersion,
    InvalidType,
    InvalidMachine,
    InvalidHeader,
    InvalidProgramHeader,
    InvalidSectionHeader,
    InvalidProgramSegment,
    OutOfMemory,
    LoadFailed,
}

/// 获取 ELF 类别
pub fn elf_class(header: &[u8; 16]) -> ElfClass {
    match header[4] {
        1 => ElfClass::ELF32,
        2 => ElfClass::ELF64,
        _ => ElfClass::None,
    }
}

/// 获取 ELF 数据编码
pub fn elf_data(header: &[u8; 16]) -> ElfData {
    match header[5] {
        1 => ElfData::LittleEndian,
        2 => ElfData::BigEndian,
        _ => ElfData::None,
    }
}

/// 获取 ELF 版本
pub fn elf_version(header: &[u8; 16]) -> u8 {
    header[6]
}

/// 验证 ELF 魔数
pub fn verify_magic(header: &[u8; 16]) -> bool {
    header[0] == 0x7F && header[1] == b'E' && header[2] == b'L' && header[3] == b'F'
}

/// 解析 32 位 ELF 文件头
pub fn parse_elf32(header: &[u8]) -> Result<Elf32Header, ElfError> {
    if header.len() < 52 {
        return Err(ElfError::InvalidHeader);
    }
    
    let ident: [u8; 16] = [
        header[0], header[1], header[2], header[3],
        header[4], header[5], header[6], header[7],
        header[8], header[9], header[10], header[11],
        header[12], header[13], header[14], header[15],
    ];
    
    if !verify_magic(&ident) {
        return Err(ElfError::InvalidMagic);
    }
    
    match elf_class(&ident) {
        ElfClass::ELF32 => {},
        _ => return Err(ElfError::InvalidClass),
    }
    
    match elf_data(&ident) {
        ElfData::LittleEndian => {},
        _ => return Err(ElfError::InvalidData),
    }
    
    if elf_version(&ident) != 1 {
        return Err(ElfError::InvalidVersion);
    }
    
    // 读取小端格式的头部
    let e_type = u16::from_le_bytes([header[16], header[17]]);
    let e_machine = u16::from_le_bytes([header[18], header[19]]);
    let e_version = u32::from_le_bytes([header[20], header[21], header[22], header[23]]);
    let e_entry = u32::from_le_bytes([header[24], header[25], header[26], header[27]]);
    let e_phoff = u32::from_le_bytes([header[28], header[29], header[30], header[31]]);
    let e_shoff = u32::from_le_bytes([header[32], header[33], header[34], header[35]]);
    let e_flags = u32::from_le_bytes([header[36], header[37], header[38], header[39]]);
    let e_ehsize = u16::from_le_bytes([header[40], header[41]]);
    let e_phentsize = u16::from_le_bytes([header[42], header[43]]);
    let e_phnum = u16::from_le_bytes([header[44], header[45]]);
    let e_shentsize = u16::from_le_bytes([header[46], header[47]]);
    let e_shnum = u16::from_le_bytes([header[48], header[49]]);
    let e_shstrndx = u16::from_le_bytes([header[50], header[51]]);
    
    Ok(Elf32Header {
        e_ident: ident,
        e_type,
        e_machine,
        e_version,
        e_entry,
        e_phoff,
        e_shoff,
        e_flags,
        e_ehsize,
        e_phentsize,
        e_phnum,
        e_shentsize,
        e_shnum,
        e_shstrndx,
    })
}

/// 解析 32 位程序头
pub fn parse_phdr32(header: &[u8], offset: usize) -> Result<Elf32Phdr, ElfError> {
    let start = offset;
    let end = start + 32;
    if header.len() < end {
        return Err(ElfError::InvalidProgramHeader);
    }
    
    let p_type = u32::from_le_bytes([header[start], header[start+1], header[start+2], header[start+3]]);
    let p_offset = u32::from_le_bytes([header[start+4], header[start+5], header[start+6], header[start+7]]);
    let p_vaddr = u32::from_le_bytes([header[start+8], header[start+9], header[start+10], header[start+11]]);
    let p_paddr = u32::from_le_bytes([header[start+12], header[start+13], header[start+14], header[start+15]]);
    let p_filesz = u32::from_le_bytes([header[start+16], header[start+17], header[start+18], header[start+19]]);
    let p_memsz = u32::from_le_bytes([header[start+20], header[start+21], header[start+22], header[start+23]]);
    let p_flags = u32::from_le_bytes([header[start+24], header[start+25], header[start+26], header[start+27]]);
    let p_align = u32::from_le_bytes([header[start+28], header[start+29], header[start+30], header[start+31]]);
    
    Ok(Elf32Phdr {
        p_type,
        p_offset,
        p_vaddr,
        p_paddr,
        p_filesz,
        p_memsz,
        p_flags,
        p_align,
    })
}

/// 验证 ELF 是否可执行
pub fn is_executable(header: &Elf32Header) -> Result<ElfType, ElfError> {
    let elf_type = match header.e_type {
        0 => ElfType::None,
        1 => ElfType::Relocatable,
        2 => ElfType::Executable,
        3 => ElfType::Shared,
        4 => ElfType::Core,
        _ => return Err(ElfError::InvalidType),
    };
    
    // 检查架构
    if header.e_machine != 3 && header.e_machine != 62 {
        return Err(ElfError::InvalidMachine);
    }
    
    // 可执行文件必须有入口点
    if elf_type == ElfType::Executable && header.e_entry == 0 {
        return Err(ElfError::InvalidHeader);
    }
    
    Ok(elf_type)
}

/// 加载 ELF 段
/// 
/// 将程序段加载到指定地址。
/// 
/// # Safety
/// 目标地址必须可写且有足够的空间。
pub unsafe fn load_segment(
    data: &[u8],
    phdr: &Elf32Phdr,
    dest: *mut u8,
) -> Result<(), ElfError> {
    if phdr.p_type != ElfPType::Load as u32 {
        return Ok(());
    }
    
    if phdr.p_filesz > phdr.p_memsz {
        return Err(ElfError::InvalidProgramSegment);
    }
    
    let src_offset = phdr.p_offset as usize;
    let dst_addr = dest.add(phdr.p_vaddr as usize);
    
    // 确保源数据足够
    if src_offset + phdr.p_filesz as usize > data.len() {
        return Err(ElfError::LoadFailed);
    }
    
    // 复制文件数据
    core::ptr::copy_nonoverlapping(
        data.as_ptr().add(src_offset),
        dst_addr,
        phdr.p_filesz as usize,
    );
    
    // 清零 BSS 段
    let bss_start = dst_addr.add(phdr.p_filesz as usize);
    let bss_size = (phdr.p_memsz - phdr.p_filesz) as usize;
    core::ptr::write_bytes(bss_start, 0, bss_size);
    
    Ok(())
}

/// 获取程序头数量
pub fn get_phdr_count(header: &Elf32Header) -> usize {
    header.e_phnum as usize
}

/// 获取程序头表偏移
pub fn get_phdr_offset(header: &Elf32Header) -> usize {
    header.e_phoff as usize
}

/// 获取程序头大小
pub fn get_phdr_size(header: &Elf32Header) -> usize {
    header.e_phentsize as usize
}

/// 打印 ELF 文件信息
pub fn print_elf_info(header: &Elf32Header) {
    crate::pr_info!("ELF: Type={:?}, Machine={}, Entry=0x{:08x}",
        header.e_type, header.e_machine, header.e_entry);
    crate::pr_info!("ELF: PHDR offset={}, count={}, size={}",
        header.e_phoff, header.e_phnum, header.e_phentsize);
}

// =============================================================================
// ELF64
// =============================================================================

/// 解析 64 位 ELF 文件头
pub fn parse_elf64(data: &[u8]) -> Result<Elf64Header, ElfError> {
    if data.len() < 64 {
        return Err(ElfError::InvalidHeader);
    }

    let ident: [u8; 16] = data[..16].try_into().unwrap();

    if !verify_magic(&ident) {
        return Err(ElfError::InvalidMagic);
    }
    if elf_class(&ident) != ElfClass::ELF64 {
        return Err(ElfError::InvalidClass);
    }
    if elf_data(&ident) != ElfData::LittleEndian {
        return Err(ElfError::InvalidData);
    }
    if elf_version(&ident) != 1 {
        return Err(ElfError::InvalidVersion);
    }

    let e_type = u16::from_le_bytes([data[16], data[17]]);
    let e_machine = u16::from_le_bytes([data[18], data[19]]);
    let e_version = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
    let e_entry = u64::from_le_bytes(data[24..32].try_into().unwrap());
    let e_phoff = u64::from_le_bytes(data[32..40].try_into().unwrap());
    let e_shoff = u64::from_le_bytes(data[40..48].try_into().unwrap());
    let e_flags = u32::from_le_bytes([data[48], data[49], data[50], data[51]]);
    let e_ehsize = u16::from_le_bytes([data[52], data[53]]);
    let e_phentsize = u16::from_le_bytes([data[54], data[55]]);
    let e_phnum = u16::from_le_bytes([data[56], data[57]]);
    let e_shentsize = u16::from_le_bytes([data[58], data[59]]);
    let e_shnum = u16::from_le_bytes([data[60], data[61]]);
    let e_shstrndx = u16::from_le_bytes([data[62], data[63]]);

    Ok(Elf64Header {
        e_ident: ident,
        e_type,
        e_machine,
        e_version,
        e_entry,
        e_phoff,
        e_shoff,
        e_flags,
        e_ehsize,
        e_phentsize,
        e_phnum,
        e_shentsize,
        e_shnum,
        e_shstrndx,
    })
}

/// 解析 64 位程序头（56 字节）
pub fn parse_phdr64(data: &[u8], offset: usize) -> Result<Elf64Phdr, ElfError> {
    let end = offset + 56;
    if data.len() < end {
        return Err(ElfError::InvalidProgramHeader);
    }
    let d = &data[offset..end];

    Ok(Elf64Phdr {
        p_type:   u32::from_le_bytes([d[0], d[1], d[2], d[3]]),
        p_flags:  u32::from_le_bytes([d[4], d[5], d[6], d[7]]),
        p_offset: u64::from_le_bytes(d[8..16].try_into().unwrap()),
        p_vaddr:  u64::from_le_bytes(d[16..24].try_into().unwrap()),
        p_paddr:  u64::from_le_bytes(d[24..32].try_into().unwrap()),
        p_filesz: u64::from_le_bytes(d[32..40].try_into().unwrap()),
        p_memsz:  u64::from_le_bytes(d[40..48].try_into().unwrap()),
        p_align:  u64::from_le_bytes(d[48..56].try_into().unwrap()),
    })
}

/// 验证 64 位 ELF 是否可执行
pub fn is_executable64(header: &Elf64Header) -> Result<ElfType, ElfError> {
    let elf_type = match header.e_type {
        2 => ElfType::Executable,
        3 => ElfType::Shared,
        _ => return Err(ElfError::InvalidType),
    };
    if header.e_machine != 62 {
        return Err(ElfError::InvalidMachine);
    }
    if elf_type == ElfType::Executable && header.e_entry == 0 {
        return Err(ElfError::InvalidHeader);
    }
    Ok(elf_type)
}

/// 快速检测是否为有效的 x86_64 ELF64
pub fn is_valid_elf64(data: &[u8]) -> bool {
    data.len() >= 64
        && data[0] == 0x7F && data[1] == b'E' && data[2] == b'L' && data[3] == b'F'
        && data[4] == 2  // ELF64
        && data[5] == 1  // LE
        && u16::from_le_bytes([data[18], data[19]]) == 62  // x86_64
}

/// PF_* → page protection 标志转换。
///
/// 每个被映射的用户段都应当是 present + user-accessible；
/// `PF_W` 决定可写（RW），`PF_X` 决定可执行（不设 NO_EXEC）。
/// 旧实现把 `PRESENT` 当成「可执行」的标志位来用——PF_X/PF_R 都置 PRESENT，
/// 从不为数据段设 NO_EXEC，于是每个段都可执行（W^X 失效）。
pub fn phdr_prot_to_flags(p_flags: u32) -> u64 {
    use crate::mm::paging::flags;
    let mut prot = flags::USER | flags::PRESENT;
    if p_flags & 2 != 0 { prot |= flags::RW; }          // PF_W → 可写
    if p_flags & 1 == 0 { prot |= flags::NO_EXEC; }     // !PF_X → 禁止取指
    prot
}

/// 检查是否为有效的 ELF 文件
pub fn is_valid_elf(data: &[u8]) -> bool {
    if data.len() < 52 {
        return false;
    }
    
    let ident: [u8; 16] = match data[..16].try_into() {
        Ok(i) => i,
        Err(_) => return false,
    };
    
    verify_magic(&ident)
        && elf_class(&ident) == ElfClass::ELF32
        && elf_data(&ident) == ElfData::LittleEndian
        && elf_version(&ident) == 1
}

/// 检查是否为 x86_64 ELF
pub fn is_x86_64(data: &[u8]) -> bool {
    if data.len() < 64 {
        return false;
    }
    data[4] == 2  // ELF64 class
        && data[5] == 1  // Little endian
        && u16::from_le_bytes([data[18], data[19]]) == 62  // x86_64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_magic_verification() {
        let valid = [0x7F, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(verify_magic(&valid));
        
        let invalid = [0x00, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(!verify_magic(&invalid));
    }

    #[test]
    fn test_elf_class() {
        let mut header = [0u8; 16];
        header[4] = 1; // ELF32
        assert_eq!(elf_class(&header), ElfClass::ELF32);
        
        header[4] = 2; // ELF64
        assert_eq!(elf_class(&header), ElfClass::ELF64);
    }

    #[test]
    fn test_elf_data() {
        let mut header = [0u8; 16];
        header[5] = 1; // Little endian
        assert_eq!(elf_data(&header), ElfData::LittleEndian);
    }
}
