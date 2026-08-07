//! USB (Universal Serial Bus) 驱动栈
//!
//! 提供 USB 主机控制器接口和设备支持。

#[cfg(feature = "extra-drivers")]
pub mod uhci;

/// USB 端点类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UsbEndpointType {
    Control = 0,
    Isochronous = 1,
    Bulk = 2,
    Interrupt = 3,
}

/// USB 传输方向
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UsbDirection {
    Out = 0,
    In = 1,
}

/// USB 设备速度
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UsbSpeed {
    Low = 0,    // 1.5 Mbps
    Full = 1,   // 12 Mbps
    High = 2,   // 480 Mbps
    Super = 3,  // 5 Gbps
    SuperPlus = 4, // 10 Gbps
}

/// USB 请求类型
#[derive(Debug, Clone, Copy)]
pub enum UsbRequestType {
    Standard = 0,
    Class = 1,
    Vendor = 2,
}

/// USB 描述符类型
#[derive(Debug, Clone, Copy)]
pub enum UsbDescriptorType {
    Device = 1,
    Config = 2,
    String = 3,
    Interface = 4,
    Endpoint = 5,
    DeviceQualifier = 6,
    OtherSpeedConfig = 7,
    InterfacePower = 8,
    Otg = 9,
    Debug = 10,
    InterfaceAssoc = 11,
}

/// USB 标准请求
#[derive(Debug, Clone, Copy)]
pub enum UsbRequest {
    GetStatus = 0,
    ClearFeature = 1,
    SetFeature = 3,
    SetAddress = 5,
    GetDescriptor = 6,
    SetDescriptor = 7,
    GetConfiguration = 8,
    SetConfiguration = 9,
    GetInterface = 10,
    SetInterface = 11,
    SynchFrame = 12,
}

/// USB 设备地址
pub const USB_DEV_ADDR_MASK: u32 = 0x7F;
pub const USB_DEV_ADDR_SHIFT: u32 = 0;

/// USB 端点地址
pub const USB_EP_ADDR_MASK: u32 = 0x0F;
pub const USB_EP_DIR_MASK: u32 = 0x80;
pub const USB_EP_DIR_SHIFT: u32 = 7;

/// USB 主机控制器类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UsbHcType {
    None,
    Uhci,   // Universal Host Controller Interface
    Ohci,   // Open Host Controller Interface  
    Ehci,   // Enhanced Host Controller Interface
    Xhci,   // eXtensible Host Controller Interface
}

/// USB 设备结构
#[derive(Debug, Clone, Copy)]
pub struct UsbDevice {
    pub address: u8,
    pub port: u8,
    pub speed: UsbSpeed,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet_size: u8,
    pub configuration: u8,
}

impl UsbDevice {
    /// 创建新设备
    pub fn new() -> Self {
        Self {
            address: 0,
            port: 0,
            speed: UsbSpeed::Full,
            vendor_id: 0,
            device_id: 0,
            class_code: 0,
            subclass: 0,
            protocol: 0,
            max_packet_size: 64,
            configuration: 0,
        }
    }
}

/// USB 端点结构
#[derive(Debug, Clone, Copy)]
pub struct UsbEndpoint {
    pub address: u8,
    pub ep_type: UsbEndpointType,
    pub direction: UsbDirection,
    pub max_packet_size: u16,
    pub interval: u8,
    pub toggle: u8,
}

impl UsbEndpoint {
    /// 创建新端点
    pub fn new(addr: u8, ep_type: UsbEndpointType, max_size: u16) -> Self {
        let dir = if (addr & 0x80) != 0 { UsbDirection::In } else { UsbDirection::Out };
        Self {
            address: addr,
            ep_type,
            direction: dir,
            max_packet_size: max_size,
            interval: 0,
            toggle: 0,
        }
    }
}

/// USB 传输请求 (URB) 状态
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UsbUrbStatus {
    Pending,
    Completed,
    Failed,
    Stalled,
    NotLinked,
    NoDevice,
}

/// USB 传输请求
#[derive(Debug, Clone)]
pub struct UsbUrb {
    pub dev: u8,
    pub ep: u8,
    pub data: *mut u8,
    pub len: usize,
    pub status: UsbUrbStatus,
    pub actual_length: usize,
}

impl UsbUrb {
    /// 创建新 URB
    pub fn new(dev: u8, ep: u8, data: *mut u8, len: usize) -> Self {
        Self {
            dev,
            ep,
            data,
            len,
            status: UsbUrbStatus::Pending,
            actual_length: 0,
        }
    }
}

/// USB 描述符头
#[repr(C)]
pub struct UsbDescriptorHeader {
    pub bLength: u8,
    pub bDescriptorType: u8,
}

/// USB 设备描述符
#[repr(C)]
pub struct UsbDeviceDescriptor {
    pub bLength: u8,
    pub bDescriptorType: u8,
    pub bcdUSB: u16,
    pub bDeviceClass: u8,
    pub bDeviceSubClass: u8,
    pub bDeviceProtocol: u8,
    pub bMaxPacketSize0: u8,
    pub idVendor: u16,
    pub idProduct: u16,
    pub bcdDevice: u16,
    pub iManufacturer: u8,
    pub iProduct: u8,
    pub iSerialNumber: u8,
    pub bNumConfigurations: u8,
}

/// USB 配置描述符
#[repr(C)]
pub struct UsbConfigDescriptor {
    pub bLength: u8,
    pub bDescriptorType: u8,
    pub wTotalLength: u16,
    pub bNumInterfaces: u8,
    pub bConfigurationValue: u8,
    pub iConfiguration: u8,
    pub bmAttributes: u8,
    pub bMaxPower: u8,
}

/// USB 接口描述符
#[repr(C)]
pub struct UsbInterfaceDescriptor {
    pub bLength: u8,
    pub bDescriptorType: u8,
    pub bInterfaceNumber: u8,
    pub bAlternateSetting: u8,
    pub bNumEndpoints: u8,
    pub bInterfaceClass: u8,
    pub bInterfaceSubClass: u8,
    pub bInterfaceProtocol: u8,
    pub iInterface: u8,
}

/// USB 端点描述符
#[repr(C)]
pub struct UsbEndpointDescriptor {
    pub bLength: u8,
    pub bDescriptorType: u8,
    pub bEndpointAddress: u8,
    pub bmAttributes: u8,
    pub wMaxPacketSize: u16,
    pub bInterval: u8,
}

impl UsbEndpointDescriptor {
    /// 获取端点类型
    pub fn get_type(&self) -> UsbEndpointType {
        match self.bmAttributes & 0x03 {
            0 => UsbEndpointType::Control,
            1 => UsbEndpointType::Isochronous,
            2 => UsbEndpointType::Bulk,
            3 => UsbEndpointType::Interrupt,
            _ => UsbEndpointType::Control,
        }
    }
    
    /// 获取方向
    pub fn get_direction(&self) -> UsbDirection {
        if (self.bEndpointAddress & 0x80) != 0 {
            UsbDirection::In
        } else {
            UsbDirection::Out
        }
    }
}

/// xHCI 寄存器偏移
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum XhciReg {
    CapLength = 0x00,
    HciVersion = 0x02,
    HcsParams1 = 0x04,
    HcsParams2 = 0x08,
    HcsParams3 = 0x0C,
    HccParams1 = 0x10,
    HccParams2 = 0x14,
    Dboff = 0x18,
    Rtsoff = 0x1C,
    UsbLegSup = 0x20,
    UsbLegCap = 0x24,
}

impl XhciReg {
    pub fn offset(&self) -> u32 {
        *self as u32
    }
}

/// xHCI Capability Parameters 1
#[derive(Debug, Clone, Copy)]
pub struct XhciHcsParams1(u32);

impl XhciHcsParams1 {
    pub fn max_slots(&self) -> u8 { ((self.0 >> 0) & 0xFF) as u8 }
    pub fn max_intrs(&self) -> u16 { ((self.0 >> 8) & 0x7FF) as u16 }
    pub fn max_ports(&self) -> u8 { ((self.0 >> 24) & 0xFF) as u8 }
}

/// xHCI Capability Parameters 2
#[derive(Debug, Clone, Copy)]
pub struct XhciHccParams1(u32);

impl XhciHccParams1 {
    pub fn max_pstreams(&self) -> u8 { ((self.0 >> 0) & 0x1F) as u8 }
    pub fn sis(&self) -> bool { ((self.0 >> 5) & 1) != 0 }
    pub fn spr(&self) -> bool { ((self.0 >> 6) & 1) != 0 }
    pub fn xecp_count(&self) -> u8 { ((self.0 >> 16) & 0xFF) as u8 }
    pub fn xecp(&self, idx: usize) -> u32 { ((self.0 >> 16) & 0xFFFF) as u32 + (idx as u32 * 4) }
}

/// xHCI 命令寄存器 (CRCR)
const XHCI_CRCR: u32 = 0x38;
const XHCI_CRCR_RCS: u32 = 1 << 0;
const XHCI_CRCR_CS: u32 = 1 << 1;
const XHCI_CRCR_CA: u32 = 1 << 2;
const XHCI_CRCR_HCRST: u32 = 1 << 3;

/// xHCI USB 状态寄存器 (USBSTS)
const XHCI_USBSTS: u32 = 0x20;
const XHCI_USBSTS_HCH: u32 = 1 << 0;
const XHCI_USBSTS_HSE: u32 = 1 << 2;
const XHCI_USBSTS_EINT: u32 = 1 << 3;

/// xHCI USB 中断使能寄存器 (USBCMD)
const XHCI_USBCMD: u32 = 0x10;
const XHCI_USBCMD_RS: u32 = 1 << 0;
const XHCI_USBCMD_HCRST: u32 = 1 << 1;
const XHCI_USBCMD_INTE: u32 = 1 << 2;
const XHCI_USBCMD_HSEE: u32 = 1 << 3;

/// USB 主机控制器状态
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UsbHcState {
    Stopped,
    Running,
    Suspended,
    Error,
}

/// USB 主机控制器
pub struct UsbHostController {
    pub hc_type: UsbHcType,
    pub base_addr: u64,
    pub io_base: u32,
    pub mem_size: u32,
    pub slots_enabled: u8,
    pub ports: u8,
    pub state: UsbHcState,
}

impl UsbHostController {
    /// 创建新的主机控制器
    pub fn new(hc_type: UsbHcType, base: u64, io: u32, size: u32) -> Self {
        Self {
            hc_type,
            base_addr: base,
            io_base: io,
            mem_size: size,
            slots_enabled: 0,
            ports: 0,
            state: UsbHcState::Stopped,
        }
    }
    
    /// 获取端口数量
    pub fn port_count(&self) -> u8 {
        self.ports
    }
}

/// USB 设备列表最大数量
pub const USB_MAX_DEVICES: usize = 128;

/// USB 全局状态
static mut USB_HOST_CTRLS: [Option<UsbHostController>; 4] = [None, None, None, None];
static mut USB_DEVICES: [Option<UsbDevice>; USB_MAX_DEVICES] = [const { None }; USB_MAX_DEVICES];

/// 初始化 USB 子系统
pub fn usb_init() {
    crate::pr_info!("USB: Initializing USB subsystem...");
    
    // 初始化主机控制器
    unsafe {
        let ptr = core::ptr::addr_of_mut!(USB_HOST_CTRLS) as *mut Option<UsbHostController>;
        for i in 0..4 {
            ptr.add(i).write(None);
        }
    }
    
    crate::pr_info!("USB: Subsystem initialized");
}

/// 获取 USB 主机控制器
pub fn get_hc(index: usize) -> Option<&'static UsbHostController> {
    if index >= 4 {
        return None;
    }
    unsafe {
        let elem_ptr = core::ptr::addr_of!(USB_HOST_CTRLS) as *const Option<UsbHostController>;
        match &*elem_ptr.add(index) {
            Some(hc) => Some(hc),
            None => None,
        }
    }
}

/// 注册 USB 主机控制器
pub fn register_hc(hc: UsbHostController) -> usize {
    let hc_type_name = match hc.hc_type {
        UsbHcType::Xhci => "xHCI",
        UsbHcType::Ehci => "EHCI",
        UsbHcType::Ohci => "OHCI",
        UsbHcType::Uhci => "UHCI",
        _ => "Unknown",
    };
    
    let hc_type_name_copy = hc_type_name;
    let hc_copy = hc;
    
    unsafe {
        let array_ptr = core::ptr::addr_of!(USB_HOST_CTRLS) as *const [Option<UsbHostController>; 4];
        let elem_ptr = array_ptr as *const Option<UsbHostController>;
        
        for i in 0..4 {
            if (*elem_ptr.add(i)).is_none() {
                let slot = &mut *(elem_ptr.add(i) as *mut Option<UsbHostController>);
                *slot = Some(hc_copy);
                crate::pr_info!("USB: Registered {} controller at index {}", hc_type_name_copy, i);
                return i;
            }
        }
    }
    0xFFFF  // 失败
}

/// 获取 USB 设备
pub fn get_device(addr: u8) -> Option<&'static UsbDevice> {
    if addr as usize >= USB_MAX_DEVICES {
        return None;
    }
    unsafe { USB_DEVICES[addr as usize].as_ref() }
}

/// 分配 USB 设备地址
pub fn alloc_device() -> Option<u8> {
    unsafe {
        for i in 1..USB_MAX_DEVICES {
            if USB_DEVICES[i].is_none() {
                USB_DEVICES[i] = Some(UsbDevice::new());
                return Some(i as u8);
            }
        }
    }
    None
}

/// 释放 USB 设备地址
pub fn free_device(addr: u8) {
    if addr as usize >= USB_MAX_DEVICES {
        return;
    }
    unsafe { USB_DEVICES[addr as usize] = None; }
}

/// 扫描 USB 主机控制器上的端口
pub fn scan_ports(_hc_index: usize) {
    crate::pr_debug!("USB: Scanning ports...");
    // TODO: 实现端口扫描
}

/// EHCI 寄存器偏移
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum EhciReg {
    CapLength = 0x00,
    HciVersion = 0x02,
    HcsParams = 0x04,
    HccParams = 0x08,
    UsbCmd = 0x10,
    UsbSts = 0x14,
    UsbIntr = 0x18,
    FrIndex = 0x1C,
    CtrdSeg = 0x20,
    PeriodicListBase = 0x24,
    AsyncListAddr = 0x28,
    ConfigFlag = 0x40,
    PortsC = 0x44,
}

impl EhciReg {
    pub fn offset(&self) -> u32 {
        *self as u32
    }
}

/// EHCI 命令寄存器位
const EHCI_CMD_RUN: u32 = 0x00000001;
const EHCI_CMD_RESET: u32 = 0x00000002;
const EHCI_CMD_PERIODIC_SCHEDULE: u32 = 0x00000010;
const EHCI_CMD_ASYNC_SCHEDULE: u32 = 0x00000020;

/// EHCI 状态寄存器位
const EHCI_STS_RUNNING: u32 = 0x00000001;
const EHCI_STS_HCHALTED: u32 = 0x00000010;
const EHCI_STS_INT: u32 = 0x00000002;

/// USB 设备类代码
pub mod device_class {
    pub const AUDIO: u8 = 0x01;
    pub const COMMUNICATIONS: u8 = 0x02;
    pub const HID: u8 = 0x03;
    pub const PHYSICAL: u8 = 0x05;
    pub const IMAGE: u8 = 0x06;
    pub const PRINTER: u8 = 0x07;
    pub const MASS_STORAGE: u8 = 0x08;
    pub const HUB: u8 = 0x09;
    pub const CDC_DATA: u8 = 0x0A;
    pub const SMART_CARD: u8 = 0x0B;
    pub const CONTENT_SECURITY: u8 = 0x0D;
    pub const VIDEO: u8 = 0x0E;
    pub const PERSONAL_HEALTHCARE: u8 = 0x0F;
    pub const AUDIO_VIDEO: u8 = 0x10;
    pub const BILLBOARD: u8 = 0x11;
    pub const USB_TYPE_C_BRIDGE: u8 = 0x12;
    pub const DIAGNOSTIC: u8 = 0xDC;
    pub const WIRELESS_CONTROLLER: u8 = 0xE0;
    pub const MISCELLANEOUS: u8 = 0xEF;
    pub const APPLICATION_SPECIFIC: u8 = 0xFE;
    pub const VENDOR_SPECIFIC: u8 = 0xFF;
}

/// 打印 USB 设备信息
pub fn print_device_info(dev: &UsbDevice) {
    crate::pr_info!(
        "USB: addr={}, port={}, speed={:?}, vendor={:04x}:{:04x}, class={:02x}:{:02x}:{:02x}",
        dev.address, dev.port, dev.speed,
        dev.vendor_id, dev.device_id,
        dev.class_code, dev.subclass, dev.protocol
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_descriptor_size() {
        let size = core::mem::size_of::<UsbDeviceDescriptor>();
        assert!(size >= 18);
    }

    #[test]
    fn test_endpoint_type() {
        let desc = UsbEndpointDescriptor {
            bLength: 7,
            bDescriptorType: 5,
            bEndpointAddress: 0x81, // EP1 IN
            bmAttributes: 0x02,      // Bulk
            wMaxPacketSize: 64,
            bInterval: 0,
        };
        assert_eq!(desc.get_type(), UsbEndpointType::Bulk);
        assert_eq!(desc.get_direction(), UsbDirection::In);
    }
}
