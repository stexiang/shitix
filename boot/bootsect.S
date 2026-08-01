/*
 * boot/bootsect.S
 *
 * 参照 linux-1.0.9 的 linux/boot/bootsect.S 重写（AT&T 语法, GNU as）。
 *
 * BIOS 把本扇区加载到 0x7C00 并跳进来。流程与原版一致：
 *   1. 把自己从 0x07C0:0 搬到 0x9000:0（让开低端内存）
 *   2. 把 setup（SETUPSECS 个扇区）读到 0x9000:0x0200
 *   3. 把 system（内核本体）读到 0x1000:0（物理 0x10000）
 *   4. 跳转到 setup
 *
 * 与原版的差异：磁盘读取用 BIOS INT 13h/AH=42h（LBA），因为镜像是
 * QEMU 的虚拟硬盘而非软驱，不需要处理 CHS 几何和分磁道读。
 */
    .code16
    .section .text
    .global _start

    .set SETUPSECS, 4           /* setup 占用的扇区数 */
    .set BOOTSEG,   0x07C0      /* BIOS 加载本扇区的位置 */
    .set INITSEG,   0x9000      /* 把自己搬到这里 */
    .set SETUPSEG,  0x9020      /* setup 的运行位置 */
    .set SYSSEG,    0x1000      /* system 的临时加载位置 */

_start:
    cli
    movw    $BOOTSEG, %ax
    movw    %ax, %ds
    movw    $INITSEG, %ax
    movw    %ax, %es
    movw    $256, %cx
    xorw    %si, %si
    xorw    %di, %di
    cld
    rep     movsw
    ljmp    $INITSEG, $go

go:
    movw    %cs, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    movw    $0x4000-12, %sp     /* 栈放在 bootsect+setup 之上 */
    sti
    movb    %dl, boot_drive     /* BIOS 在 dl 里给出启动盘号 */

    movw    $msg1, %si
    call    prtstr

/* 读 setup 到 0x9000:0x0200 */
    movl    $1, cur_lba
    movw    $INITSEG, %di
    movw    $0x0200, dest_off
    movw    $SETUPSECS, %cx
    call    read_sectors

/* 读 system 到 0x1000:0 */
    movw    $0, dest_off
    movw    syssize, %ax        /* 单位: 16 字节 clicks, 由 build 脚本填写 */
    addw    $31, %ax
    shrw    $5, %ax             /* 换算成扇区数 */
    movw    %ax, %cx
    movw    $SYSSEG, %di
    movl    $(1 + SETUPSECS), cur_lba
load_loop:
    testw   %cx, %cx
    jz      load_done
    movw    %cx, %ax
    cmpw    $64, %ax            /* 每次最多 64 扇区 = 32KB */
    jbe     1f
    movw    $64, %ax
1:  pushw   %cx
    pushw   %ax
    movw    %ax, %cx
    call    read_sectors
    popw    %ax
    popw    %cx
    subw    %ax, %cx
    movw    %ax, %bx
    shlw    $5, %bx             /* 扇区数 * 512 / 16 = 段增量 */
    addw    %bx, %di
    movzwl  %ax, %ebx
    addl    %ebx, cur_lba
    movb    $'.', %al
    call    prtchr
    jmp     load_loop
load_done:
    call    kill_motor
    movw    $msg2, %si
    call    prtstr
    ljmp    $SETUPSEG, $0

/*
 * read_sectors: 用 INT 13h/AH=42h 读盘
 *   入口: %cx = 扇区数, %di = 目标段, cur_lba = 起始 LBA, dest_off = 目标偏移
 */
read_sectors:
    pushal
    movw    $0x0010, dap_size   /* size=0x10, reserved=0 */
    movw    %cx, dap_count
    movw    dest_off, %ax
    movw    %ax, dap_off
    movw    %di, dap_seg
    movl    cur_lba, %eax
    movl    %eax, dap_lba
    movl    $0, dap_lba + 4
    movw    $3, retry
1:  movw    $dap, %si
    movb    $0x42, %ah
    movb    boot_drive, %dl
    int     $0x13
    jnc     2f
    decw    retry
    jz      disk_error
    xorb    %ah, %ah            /* 复位磁盘控制器后重试 */
    movb    boot_drive, %dl
    int     $0x13
    jmp     1b
2:  popal
    ret

disk_error:
    movw    $msg_err, %si
    call    prtstr
1:  hlt
    jmp     1b

/* 关掉软驱马达，让内核在已知状态下启动（同原版 kill_motor） */
kill_motor:
    pushw   %dx
    movw    $0x03f2, %dx
    xorb    %al, %al
    outb    %al, %dx
    popw    %dx
    ret

prtstr:
    lodsb
    testb   %al, %al
    jz      1f
    call    prtchr
    jmp     prtstr
1:  ret

prtchr:
    pushw   %bx
    movb    $0x0e, %ah
    movw    $0x0007, %bx
    int     $0x10
    popw    %bx
    ret

/* ---- 数据 ---- */
boot_drive:  .byte 0x80
retry:       .word 0
dest_off:    .word 0
cur_lba:     .long 0

dap:
dap_size:    .word 0
dap_count:   .word 0
dap_off:     .word 0
dap_seg:     .word 0
dap_lba:     .quad 0

msg1:        .ascii "Loading"
             .byte  0
msg2:        .byte  13, 10
             .ascii "Booting shitix"
             .byte  13, 10, 0
msg_err:     .byte  13, 10
             .ascii "Disk error"
             .byte  13, 10, 0

/* 与原版一致的尾部参数区 */
    .org 498
root_flags:  .word 0
syssize:     .word 0             /* build 脚本填入 system 的 clicks 数 */
swap_dev:    .word 0
ram_size:    .word 0
vid_mode:    .word 0
root_dev:    .word 0
boot_flag:   .word 0xAA55
