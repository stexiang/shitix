//! E820 内存图访问。
//!
//! 原版 linux-1.0.9 没有这个——它只用 `int 15h/88h` 的一个 KB 数
//! （`mm/memory.c` 的 `mem_init` 靠 `end_mem` 参数），最多 64MB。
//! setup.S 已经收集了完整的 E820 表，这里把它包装成迭代器给 mm 用。

/// E820 条目数存放地址（setup.S 写入）。
const COUNT_ADDR: usize = 0x9_01E0;
/// 条目数组基址，20 字节一条。
const BASE: usize = 0x9_F000;
/// setup.S 最多写这么多条。
const MAX: usize = 128;
/// E820 类型 1 = 可用 RAM。
const TYPE_USABLE: u32 = 1;

/// 一条 E820 记录。
#[derive(Copy, Clone)]
pub struct Entry {
    pub base: u64,
    pub len: u64,
    pub kind: u32,
}

impl Entry {
    pub fn is_usable(&self) -> bool {
        self.kind == TYPE_USABLE
    }
}

/// 条目数，已按 [`MAX`] 截断以防参数区被污染。
pub fn count() -> usize {
    // SAFETY: setup.S 在这个恒等映射的低端地址写了 u16 条目数。
    let raw = unsafe { core::ptr::read_volatile(COUNT_ADDR as *const u16) } as usize;
    raw.min(MAX)
}

/// 读第 `i` 条，越界返回 `None`。
pub fn get(i: usize) -> Option<Entry> {
    if i >= count() {
        return None;
    }
    let p = (BASE + i * 20) as *const u8;
    // SAFETY: i < count() <= 128，故 p..p+20 落在 0x9F000..0x9FA00 内，
    // 属于 setup.S 写入且被恒等映射的低端内存。20 字节步进不保证 8 字节
    // 对齐，故用 read_unaligned。
    unsafe {
        Some(Entry {
            base: (p as *const u64).read_unaligned(),
            len: (p.add(8) as *const u64).read_unaligned(),
            kind: (p.add(16) as *const u32).read_unaligned(),
        })
    }
}

/// 遍历全部条目。
pub fn iter() -> impl Iterator<Item = Entry> + Clone {
    (0..count()).filter_map(get)
}

/// 只遍历可用区间，产出 `(base, len)`，供 [`crate::mm::init`] 使用。
pub fn usable() -> impl Iterator<Item = (u64, u64)> + Clone {
    iter().filter(|e| e.is_usable()).map(|e| (e.base, e.len))
}

/// 可用内存总字节数。
pub fn usable_bytes() -> u64 {
    usable().fold(0u64, |acc, (_, len)| acc.saturating_add(len))
}
