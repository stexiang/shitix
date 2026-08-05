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

    .bss : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    _kernel_end = .;

    /*
     * 内核镜像（含 BSS）不能长到 0x90000：setup.S 把机器参数留在
     * 0x90000、E820 表留在 0x9E000，start_kernel 还要读它们。BSS 盖过去
     * 的话在 mm 初始化之前就崩，而且症状（内存大小变成垃圾值）看不出
     * 是链接布局的问题。
     *
     * 同类事故已经发生过两次：一次是 BSS 盖掉 0x70000 的四级页表导致
     * 三重错误（页表后来搬到 0x4000），一次是内核栈静态池把 BSS 顶过
     * 0x90000。所以在链接期就拦住。
     */
    ASSERT(_kernel_end <= 0x90000,
           "kernel image (incl. BSS) overlaps setup parameter area at 0x90000")

    /DISCARD/ : {
        *(.note*)
        *(.comment*)
        *(.eh_frame*)
        *(.gcc_except_table*)
    }
}
