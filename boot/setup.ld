/* setup 运行在 0x9020:0，段内偏移从 0 开始。 */
ENTRY(_setup_start)
SECTIONS
{
    . = 0;
    .text : { *(.text) *(.text.*) *(.rodata*) *(.data*) }
    /DISCARD/ : { *(.note*) *(.comment) *(.eh_frame*) }
}
