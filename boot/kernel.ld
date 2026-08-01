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

    /DISCARD/ : {
        *(.note*)
        *(.comment*)
        *(.eh_frame*)
        *(.gcc_except_table*)
    }
}
