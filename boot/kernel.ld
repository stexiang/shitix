/* system（head.S + Rust 内核）被 bootsect 加载到物理 0x10000，
   long mode 下低 1GB 是恒等映射，所以链接地址 == 物理地址。 */
ENTRY(_head_start)

SECTIONS
{
    . = 0x10000;

    .text : {
        *(.text.head)
        *(.text .text.*)
    }

    .rodata : ALIGN(16) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(16) {
        *(.data .data.*)
        *(.got .got.plt)
    }

    _image_end = .;

    /*
     * BSS 不放装载镜像里（objcopy -R .bss），由 head.S 上电清零，
     * 所以它可以待在装载区之外。挪到 1MB：装载镜像（text/rodata/data）
     * 从 0x10000 线性铺开，必须止于 0x90000 的参数区之前；而 BSS 若跟在
     * 后面，体积一大就会盖掉 0x90000 机器参数 / 0x90200 setup / 0x9E000
     * E820 表。1MB 之上是恒等映射的干净 RAM，mm::init 又把
     * mem_map/引用计数表/内核栈池排在 _kernel_end 之后，不会冲突。
     */
    . = 0x100000;
    .bss : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    _kernel_end = .;

    /* 装载镜像（objcopy 出来的 system.bin）加载到 0x10000，不能碰 0x90000 */
    ASSERT(_image_end <= 0x90000,
           "kernel image (text/rodata/data) extends past safe area at 0x90000")
    /* BSS 顶到 2.5MB 之前要停下，给 mem_map/引用计数表/内核栈池留地方。
     * 这些表是按 kernel_end 动态往后排的（见 page_alloc.rs），这里只是
     * 留余量的护栏：256MB 内存下 mem_map+ref+kstack 合计 <1MB。 */
    ASSERT(_kernel_end <= 0x280000,
           "kernel BSS extends past 0x280000")

    /DISCARD/ : {
        *(.note*)
        *(.comment*)
        *(.eh_frame*)
        *(.gcc_except_table*)
    }
}
