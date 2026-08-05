//! PCI 总线枚举和设备发现模块
//! 
//! 提供 PCI 配置空间访问和设备枚举功能。

/// PCI 配置空间地址寄存器
const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

/// PCI 供应商/设备 ID 寄存器
const PCI_VENDOR_ID: u8 = 0x00;
/// PCI 设备 ID 寄存器
const PCI_DEVICE_ID: u8 = 0x02;
/// PCI 命令寄存器
const PCI_COMMAND: u8 = 0x04;
/// PCI 状态寄存器
const PCI_STATUS: u8 = 0x06;
/// PCI 修订 ID 寄存器
const PCI_REVISION_ID: u8 = 0x08;
/// PCI 编程接口寄存器
const PCI_PROG_IF: u8 = 0x09;
/// PCI 子系统厂商寄存器
const PCI_SUBVENDOR_ID: u8 = 0x2C;
/// PCI 子系统 ID 寄存器
const PCI_SUBSYSTEM_ID: u8 = 0x2E;
/// PCI 能力指针寄存器
const PCI_CAPABILITY_POINTER: u8 = 0x34;

/// 最大 PCI 设备数
const MAX_PCI_DEVICES: usize = 32;

/// PCI 配置空间头
#[repr(C, packed)]
pub struct PciHeader {
    pub vendor_id: u16,
    pub device_id: u16,
    pub command: u16,
    pub status: u16,
    pub revision: u8,
    pub prog_if: u8,
    pub subclass: u8,
    pub class_code: u8,
    pub cache_line_size: u8,
    pub latency_timer: u8,
    pub header_type: u8,
    pub bist: u8,
}

/// PCI BAR (Base Address Register) 信息
#[derive(Debug, Clone, Copy)]
pub struct PciBar {
    pub base: u64,
    pub size: u64,
    pub is_io: bool,
    pub is_64bit: bool,
    pub is_prefetchable: bool,
}

/// PCI 设备信息
#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub header_type: u8,
    pub bars: [Option<PciBar>; 6],
}

impl PciDevice {
    /// 获取设备描述
    pub fn description(&self) -> &'static str {
        match (self.class_code, self.subclass) {
            (0x01, 0x01) => "IDE Controller",
            (0x01, 0x06) => "SATA Controller",
            (0x01, 0x08) => "NVMe Controller",
            (0x02, 0x00) => "Ethernet Controller",
            (0x02, 0x80) => "Network Controller",
            (0x03, 0x00) => "VGA Controller",
            (0x04, 0x00) => "Audio Controller",
            (0x06, 0x00) => "Host Bridge",
            (0x06, 0x01) => "ISA Bridge",
            (0x06, 0x04) => "PCI Bridge",
            (0x0C, 0x03) => "USB Controller",
            (0x0C, 0x30) => "USB Controller (xHCI)",
            _ => "Unknown Device",
        }
    }
}

/// 读取 PCI 配置空间的 32 位值（地址直接传递）
fn pci_config_read32(addr: u32) -> u32 {
    unsafe {
        // 写入地址
        let addr_val = addr | 0x80000000u32;
        core::arch::asm!(
            "out 0xCF8, eax",
            in("eax") addr_val,
            options(nostack, nomem, preserves_flags)
        );
        // 读取数据
        let result: u32;
        core::arch::asm!(
            "in eax, 0xCFC",
            out("eax") result,
            options(nostack, nomem, preserves_flags)
        );
        result
    }
}

/// 写入 PCI 配置空间的 32 位值（地址直接传递）
fn pci_config_write32(addr: u32, value: u32) {
    unsafe {
        core::arch::asm!(
            "out 0xCF8, eax",
            in("eax") addr | 0x80000000u32,
            options(nostack, nomem, preserves_flags)
        );
        core::arch::asm!(
            "out 0xCFC, eax",
            in("eax") value,
            options(nostack, nomem, preserves_flags)
        );
    }
}

/// 读取 PCI 配置空间的 16 位值
fn pci_read16(bus: u8, dev: u8, func: u8, offset: u8) -> u16 {
    let addr = ((bus as u32) << 16) | ((dev as u32) << 11) | ((func as u32) << 8) | (offset as u32 & 0xFC);
    (pci_config_read32(addr) >> ((offset & 2) * 8)) as u16
}

/// 读取 PCI 配置空间的 8 位值
fn pci_read8(bus: u8, dev: u8, func: u8, offset: u8) -> u8 {
    let addr = ((bus as u32) << 16) | ((dev as u32) << 11) | ((func as u32) << 8) | (offset as u32 & 0xFC);
    (pci_config_read32(addr) >> ((offset & 3) * 8)) as u8
}

/// 写入 16 位 PCI 配置值
fn pci_write16(bus: u8, dev: u8, func: u8, offset: u8, value: u16) {
    let addr = ((bus as u32) << 16) | ((dev as u32) << 11) | ((func as u32) << 8) | (offset as u32 & 0xFC);
    let current = pci_config_read32(addr);
    let shift = ((offset & 2) * 8) as u32;
    let new_value = (current & !(0xFFFFu32 << shift)) | ((value as u32) << shift);
    pci_config_write32(addr, new_value);
}

/// 检查 PCI 设备是否存在
pub fn pci_device_exists(bus: u8, dev: u8, func: u8) -> bool {
    let vendor_id = pci_read16(bus, dev, func, PCI_VENDOR_ID);
    vendor_id != 0xFFFF
}

/// 获取 PCI BAR
fn get_bar(bus: u8, dev: u8, func: u8, bar_index: usize) -> Option<PciBar> {
    let offset = PCI_VENDOR_ID + 4 + (bar_index as u8 * 4);
    let base = pci_read16(bus, dev, func, offset) as u32;
    
    if base == 0 {
        return None;
    }
    
    let is_io = (base & 0x01) != 0;
    
    // 写入全 1 来获取大小
    let addr = ((bus as u32) << 16) | ((dev as u32) << 11) | ((func as u32) << 8) | (offset as u32 & 0xFC);
    pci_config_write32(addr, 0xFFFFFFFF);
    let size_raw = pci_config_read32(addr);
    pci_config_write32(addr, base);
    
    if is_io {
        let size_mask = !0x03u32;
        Some(PciBar {
            base: (base & !0x03) as u64,
            size: (!(size_raw & size_mask) & size_mask) as u64 + 1,
            is_io: true,
            is_64bit: false,
            is_prefetchable: false,
        })
    } else {
        let size_mask = !0x0Fu32;
        let is_64bit = (base & 0x06) == 0x04 && bar_index < 5;
        Some(PciBar {
            base: (base & !0x0F) as u64,
            size: (!(size_raw & size_mask) & size_mask) as u64 + 1,
            is_io: false,
            is_64bit,
            is_prefetchable: (base & 0x08) != 0,
        })
    }
}

/// 发现所有 PCI 设备
pub fn pci_enumerate() -> [Option<PciDevice>; MAX_PCI_DEVICES] {
    let mut devices = [None; MAX_PCI_DEVICES];
    let mut count = 0;
    
    for bus in 0..=255 {
        for dev in 0..32 {
            if count >= MAX_PCI_DEVICES {
                break;
            }
            // 检查设备是否存在
            let vendor_id = pci_read16(bus, dev, 0, PCI_VENDOR_ID);
            if vendor_id == 0xFFFF {
                continue;
            }
            
            // 获取功能数量
            let header_type = pci_read8(bus, dev, 0, PCI_VENDOR_ID + 0x0D);
            let max_func = if (header_type & 0x80) != 0 { 8 } else { 1 };
            
            for func in 0..max_func {
                if count >= MAX_PCI_DEVICES {
                    break;
                }
                if !pci_device_exists(bus, dev, func) {
                    continue;
                }
                
                let device_id = pci_read16(bus, dev, func, PCI_DEVICE_ID);
                let class_code = pci_read8(bus, dev, func, PCI_VENDOR_ID + 0x0B);
                let subclass = pci_read8(bus, dev, func, PCI_VENDOR_ID + 0x0A);
                let prog_if = pci_read8(bus, dev, func, PCI_VENDOR_ID + 0x09);
                let header = pci_read8(bus, dev, func, PCI_VENDOR_ID + 0x0E);
                
                // 获取 BAR
                let mut bars = [None; 6];
                let num_bars = if header == 0 { 6 } else { 2 };
                for i in 0..num_bars {
                    bars[i] = get_bar(bus, dev, func, i);
                }
                
                devices[count] = Some(PciDevice {
                    bus,
                    device: dev,
                    function: func,
                    vendor_id,
                    device_id,
                    class_code,
                    subclass,
                    prog_if,
                    header_type: header,
                    bars,
                });
                count += 1;
            }
        }
    }
    
    devices
}

/// PCI 设备类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PciDeviceType {
    HostBridge,
    PciBridge,
    IsaBridge,
    IdeController,
    SataController,
    NvmeController,
    EthernetController,
    UsbController,
    VgaController,
    AudioController,
    Unknown,
}

/// 获取设备类型
pub fn get_device_type(device: &PciDevice) -> PciDeviceType {
    match (device.class_code, device.subclass) {
        (0x06, 0x00) => PciDeviceType::HostBridge,
        (0x06, 0x01) => PciDeviceType::IsaBridge,
        (0x06, 0x04) => PciDeviceType::PciBridge,
        (0x01, 0x01) => PciDeviceType::IdeController,
        (0x01, 0x06) => PciDeviceType::SataController,
        (0x01, 0x08) => PciDeviceType::NvmeController,
        (0x02, 0x00) => PciDeviceType::EthernetController,
        (0x0C, 0x03) => PciDeviceType::UsbController,
        (0x0C, 0x30) => PciDeviceType::UsbController,
        (0x03, 0x00) => PciDeviceType::VgaController,
        (0x04, 0x00) => PciDeviceType::AudioController,
        _ => PciDeviceType::Unknown,
    }
}

/// 启用 PCI 设备
pub fn enable_device(bus: u8, dev: u8, func: u8) {
    let command = pci_read16(bus, dev, func, PCI_COMMAND);
    pci_write16(bus, dev, func, PCI_COMMAND, command | 0x07);
}

/// 禁用 PCI 设备
pub fn disable_device(bus: u8, dev: u8, func: u8) {
    let command = pci_read16(bus, dev, func, PCI_COMMAND);
    pci_write16(bus, dev, func, PCI_COMMAND, command & !0x07);
}

/// 查找特定类型的第一个 PCI 设备
pub fn pci_find_device(class: u8, subclass: u8) -> Option<PciDevice> {
    let devices = pci_enumerate();
    for opt in devices.iter() {
        if let Some(device) = opt {
            if device.class_code == class && device.subclass == subclass {
                return Some(device.clone());
            }
        }
    }
    None
}

/// 查找 USB 控制器
pub fn pci_find_usb_controller() -> Option<PciDevice> {
    if let Some(device) = pci_find_device(0x0C, 0x30) { return Some(device); }
    if let Some(device) = pci_find_device(0x0C, 0x03) { return Some(device); }
    if let Some(device) = pci_find_device(0x0C, 0x10) { return Some(device); }
    pci_find_device(0x0C, 0x00)
}

/// 查找以太网控制器
pub fn pci_find_ethernet() -> Option<PciDevice> {
    pci_find_device(0x02, 0x00)
}

/// 查找 NVMe 控制器
pub fn pci_find_nvme() -> Option<PciDevice> {
    pci_find_device(0x01, 0x08)
}

/// 查找 SATA 控制器
pub fn pci_find_sata() -> Option<PciDevice> {
    if let Some(device) = pci_find_device(0x01, 0x06) {
        return Some(device);
    }
    pci_find_device(0x01, 0x01)
}

/// PCI 初始化
pub fn pci_init() {
    crate::pr_info!("PCI: Initializing PCI bus...");
    
    // 检查 PCI 控制器是否存在
    let config = pci_config_read32(0);
    if config == 0xFFFFFFFF {
        crate::pr_warn!("PCI: No PCI controller found!");
        return;
    }
    
    // 枚举所有设备
    let devices = pci_enumerate();
    let count = devices.iter().filter(|d| d.is_some()).count();
    
    if count == 0 {
        crate::pr_info!("PCI: No PCI devices found");
        return;
    }
    
    crate::pr_info!("PCI: Found {} device(s)", count);
    
    for opt in devices.iter() {
        if let Some(device) = opt {
            let dev_type = get_device_type(device);
            crate::pr_info!(
                "PCI: {:02x}:{:02x}.{:x} {:04x}:{:04x} {:?} ({})",
                device.bus, device.device, device.function,
                device.vendor_id, device.device_id,
                dev_type, device.description()
            );
        }
    }
    
    crate::pr_info!("PCI: Initialization complete");
}

/// 扫描 PCI 总线并打印所有设备
pub fn pci_scan() {
    crate::pr_info!("PCI: Scanning bus...");
    
    let devices = pci_enumerate();
    let count = devices.iter().filter(|d| d.is_some()).count();
    crate::pr_info!("PCI: Found {} device(s)", count);
    
    for opt in devices.iter() {
        if let Some(device) = opt {
            crate::pr_info!(
                "  {:02x}:{:02x}.{:x} {} {:04x}:{:04x}",
                device.bus, device.device, device.function,
                device.description(),
                device.vendor_id, device.device_id
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pci_register_offsets() {
        assert_eq!(PCI_VENDOR_ID, 0x00);
        assert_eq!(PCI_DEVICE_ID, 0x02);
        assert_eq!(PCI_COMMAND, 0x04);
        assert_eq!(PCI_STATUS, 0x06);
    }
}
