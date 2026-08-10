/* Minimal dynamically-linked: _start with inline syscall, no libc at all */
void _start(void) {
    register long rax __asm__("rax") = 1;   /* SYS_write */
    register long rdi __asm__("rdi") = 1;   /* fd=1 */
    register long rsi __asm__("rsi");        /* buf */
    register long rdx __asm__("rdx") = 18;   /* len */
    __asm__ volatile (
        "lea msg(%%rip), %0\n\t"
        "syscall"
        : "=r"(rsi)
        : "r"(rax), "r"(rdi), "r"(rdx)
        : "rcx", "r11", "memory"
    );
    /* exit(0) */
    __asm__ volatile (
        "mov $60, %%rax\n\t"  /* SYS_exit */
        "xor %%rdi, %%rdi\n\t"
        "syscall"
        : : : "rax", "rdi", "rcx", "r11"
    );
    __asm__(".section .rodata; msg: .ascii \"RAW_DYN_HELLO_OK\\n\"");
}
