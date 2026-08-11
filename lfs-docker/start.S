/* Minimal _start: write "DYN_OK\n" then exit */
.text
.globl _start
_start:
    /* write(1, msg, 7) */
    movl $1, %eax       /* SYS_write on x86_64 */
    movl $1, %edi       /* fd=1 */
    leaq msg(%rip), %rsi
    movl $7, %edx       /* len=7 */
    syscall

    /* exit(0) */
    movl $60, %eax      /* SYS_exit */
    xorl %edi, %edi
    syscall

.section .rodata
msg:
    .ascii "DYN_OK\n"
