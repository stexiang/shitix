//! 启动命令行解析与根/init 设备解析。
//!
//! 对应原版 `init/main.c` 开头的 `setup_arch()` / `parse_options()`：
//! 从启动命令行里取 `root=`、`init=`、`console=`、`ro`/`rw` 等选项。
//!
//! 命令行由 `boot/setup.S` 拷到固定物理地址 [`CMDLINE_PHYS`]，`start_kernel`
//! 读出来存进 [`BOOT_PARAMS`]（等价原版 `saved_command_line`）。

use crate::fs::mkdev;
use crate::drivers::block::major::HD_MAJOR;

/// 解析后的启动参数。等价原版 `ROOT_DEV`、`execute_command`、`root_mountflags`。
pub struct BootConfig {
    /// 根设备号（major<<8 | minor）。0 表示未指定，由默认逻辑兜底。
    pub root_dev: u16,
    /// init 程序路径（NUL 结尾）。默认 "/sbin/init"。
    pub init_path: [u8; 64],
    pub init_len: usize,
    /// 根挂载是否只读。
    pub root_rdonly: bool,
    /// console 设备名（未实现完整解析，仅记录）。
    pub console: [u8; 16],
    pub console_len: usize,
    /// 原始命令行（NUL 结尾），供调试打印。
    pub cmdline: [u8; 512],
    pub cmdline_len: usize,
}

static mut BOOT_CONFIG: BootConfig = BootConfig::empty();

impl BootConfig {
    const fn empty() -> Self {
        BootConfig {
            root_dev: 0,
            init_path: [0; 64],
            init_len: 0,
            root_rdonly: true,
            console: [0; 16],
            console_len: 0,
            cmdline: [0; 512],
            cmdline_len: 0,
        }
    }

    /// init 程序路径（含末尾 NUL，便于直接当 C 字符串）。
    pub fn init_str(&self) -> &[u8] {
        let n = self.init_len.min(self.init_path.len() - 1);
        &self.init_path[..n]
    }
}

/// 解析后的启动参数的全局引用。等价原版 `saved_command_line` 指向的静态串。
pub fn config() -> &'static BootConfig {
    // SAFETY: 启动期单线程写入一次（`parse`），之后只读。
    unsafe { &*core::ptr::addr_of!(BOOT_CONFIG) }
}

/// 把 `/dev/hdaN` 这类名字解析成 `(major, minor)`。
/// 支持的命名（与 Linux 老式 IDE 约定一致）：
///   /dev/hda, /dev/hda1..hda4  -> major 3, minor 0..4
///   /dev/hdb, /dev/hdb1..hdb4  -> major 3, minor 16..20
///   /dev/ram, /dev/ram0        -> major 1, minor 1
/// 也接受裸的 `0xNNMM`（major<<8|minor）十六进制，对应原版 `root=0x301` 写法。
fn parse_root_dev(s: &[u8]) -> Option<u16> {
    // 去掉可选的 /dev/ 前缀
    let s = s.strip_prefix(b"/dev/").unwrap_or(s);

    // 裸十六进制：0xNNMM
    if s.starts_with(b"0x") || s.starts_with(b"0X") {
        let hex = &s[2..];
        let mut v: u16 = 0;
        if hex.is_empty() {
            return None;
        }
        for &b in hex {
            let d = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return None,
            };
            v = v.wrapping_mul(16).wrapping_add(d as u16);
        }
        return Some(v);
    }

    // /dev/hda / hdb ...
    if s.len() >= 3 && &s[..2] == b"hd" {
        let drive_letter = s[2];
        let drive = match drive_letter {
            b'a' => 0u16,
            b'b' => 1,
            _ => return None,
        };
        // 分区号可选：hda = 整盘(minor 0)，hda1..hda4
        let part = if s.len() == 3 {
            0u16
        } else {
            // 只取第一个数字字符（不支持 >9 的分区号，老式 MBR 主分区只到 4）
            let c = s[3];
            if !(b'1'..=b'9').contains(&c) {
                return None;
            }
            (c - b'0') as u16
        };
        // minor = drive*16 + part（与 Linux IDE 主从/分区编号一致）
        let minor = drive * 16 + part;
        return Some(mkdev(HD_MAJOR as u32, minor as u32));
    }

    // /dev/ram0 / /dev/ram -> ramdisk (1,1)
    if s.starts_with(b"ram") {
        return Some(mkdev(1, 1));
    }

    None
}

/// 从 `boot/setup.S` 拷到 [`CMDLINE_PHYS`] 的命令行。
pub const CMDLINE_PHYS: usize = 0x91000;
/// 命令行缓冲区最大长度。
pub const CMDLINE_MAX: usize = 0x800;

/// 把 setup 留在 [`CMDLINE_PHYS`] 的命令行读出来并解析。
///
/// 对应原版 `start_kernel` → `setup_arch` 里 `strlcpy(command_line, ...)` 之后
/// 的 `parse_options(command_line)`。
///
/// # Safety
/// 启动期调用一次；`CMDLINE_PHYS` 指向 setup.S 写好的 NUL 结尾命令行，
/// 位于低 1MB 恒等映射范围内。
pub unsafe fn parse() {
    unsafe {
        let cfg = &mut *core::ptr::addr_of_mut!(BOOT_CONFIG);
        // 默认值
        cfg.root_dev = 0;
        cfg.root_rdonly = true;
        cfg.init_len = 0;
        cfg.console_len = 0;
        cfg.cmdline_len = 0;

        // 拷贝命令行到静态缓冲区（限长）
        let src = CMDLINE_PHYS as *const u8;
        let mut i = 0;
        while i < CMDLINE_MAX.min(cfg.cmdline.len() - 1) {
            let b = core::ptr::read_volatile(src.add(i));
            if b == 0 {
                break;
            }
            cfg.cmdline[i] = b;
            i += 1;
        }
        cfg.cmdline_len = i;

        // 默认 init
        set_init(cfg, b"/sbin/init");

        // 没有命令行就用默认值
        if cfg.cmdline_len == 0 {
            cfg.root_rdonly = true;
            return;
        }

        // 拷一份到栈上解析，避免 cfg 的可变借用与 cmdline 切片的共享借用冲突。
        let mut local: [u8; 512] = [0; 512];
        let n = cfg.cmdline_len.min(local.len());
        local[..n].copy_from_slice(&cfg.cmdline[..n]);
        let line = &local[..n];

        // 逐 token 解析（空格分隔）
        let mut start = 0;
        let n = line.len();
        while start < n {
            // 跳过空格
            while start < n && (line[start] == b' ' || line[start] == b'\t') {
                start += 1;
            }
            if start >= n {
                break;
            }
            let mut end = start;
            while end < n && line[end] != b' ' && line[end] != b'\t' {
                end += 1;
            }
            let tok = &line[start..end];
            apply_token(cfg, tok);
            start = end;
        }
    }
}

fn set_init(cfg: &mut BootConfig, s: &[u8]) {
    let n = s.len().min(cfg.init_path.len() - 1);
    cfg.init_path[..n].copy_from_slice(&s[..n]);
    cfg.init_path[n] = 0;
    cfg.init_len = n;
}

fn set_console(cfg: &mut BootConfig, s: &[u8]) {
    let n = s.len().min(cfg.console.len() - 1);
    cfg.console[..n].copy_from_slice(&s[..n]);
    cfg.console[n] = 0;
    cfg.console_len = n;
}

/// 应用单个 token。等价原版 `parse_options` 里对每个 `opt` 的判断。
fn apply_token(cfg: &mut BootConfig, tok: &[u8]) {
    if let Some(val) = strip_prefix(tok, b"root=") {
        if let Some(dev) = parse_root_dev(val) {
            cfg.root_dev = dev;
        }
        return;
    }
    if let Some(val) = strip_prefix(tok, b"init=") {
        set_init(cfg, val);
        return;
    }
    if let Some(val) = strip_prefix(tok, b"console=") {
        set_console(cfg, val);
        return;
    }
    if tok == b"ro" {
        cfg.root_rdonly = true;
        return;
    }
    if tok == b"rw" {
        cfg.root_rdonly = false;
        return;
    }
    // 未识别的 token 忽略（原版同样静默丢弃）
}

fn strip_prefix<'a>(s: &'a [u8], p: &[u8]) -> Option<&'a [u8]> {
    if s.len() >= p.len() && &s[..p.len()] == p {
        Some(&s[p.len()..])
    } else {
        None
    }
}
