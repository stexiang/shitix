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
     * 0x90000、E820 表留在 0x9E000，start_kernel 还要读它们。
     *
     * setup 代码在 0x90200-0x90A00（4 sectors），启动后不再使用。
     * E820 条目在 0x9E000。BSS 可以使用 0x90C00-0x9DFFF 的空间。
     * 此处留 4KB 安全余量到 E820 表。
     */
    ASSERT(_kernel_end <= 0x9F000,
           "kernel image (incl. BSS) extends past safe area at 0x9F000")

    /DISCARD/ : {
        *(.note*)
        *(.comment*)
        *(.eh_frame*)
        *(.gcc_except_table*)
    }
}
