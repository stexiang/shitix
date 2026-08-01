/* bootsect 被 BIOS 加载到 0x7C00，但立刻自搬到 0x9000:0 并在那里运行，
   所有标签都是段内偏移，所以基址取 0。 */
ENTRY(_start)
SECTIONS
{
    . = 0;
    .text : { *(.text) *(.text.*) *(.rodata*) *(.data*) }
    /DISCARD/ : { *(.note*) *(.comment) *(.eh_frame*) }
}
