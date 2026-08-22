/*
 * shitix 最小 /init —— 启动一个交互式 /bin/sh 并保持存活（不立即退出）。
 *
 * 完全自包含：自定义 _start + 内联 syscall 汇编，不链接任何 libc
 * （gcc -nostdlib -static）。这样静态 ELF 不依赖 libc 的启动路径 / TLS /
 * errno 初始化，在这个内核上最稳。
 *
 * 行为：
 *   1. 打印 banner
 *   2. fork 一个子进程 exec /bin/sh -i（强制交互式 shell）
 *   3. 父进程（PID 1）wait4 回收 shell；shell 退出后重新拉起（respawn）
 *      这样 init 永不退出 —— 即「runs sh and doesn't exit immediately」
 *
 * 注意：内核只支持 ELF（无 #! shebang 支持），所以 /init 必须是编译出的
 * ELF 二进制，不能是 shell 脚本。
 */

#define SYS_write  1
#define SYS_ioctl  16
#define SYS_fork   57
#define SYS_execve 59
#define SYS_exit   60
#define SYS_wait4  61
#define SYS_setsid 112

/* 作业控制 ioctl（见内核 tty 层） */
#define TIOCSCTTY 0x540E

#define WNOHANG 1

/* ---- 内联 syscall 包装（x86_64：号在 rax，参数 rdi/rsi/rdx/r10/...） ---- */
static inline long sys0(long n) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n) : "rcx", "r11", "memory");
    return r;
}
static inline long sys1(long n, long a1) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a1) : "rcx", "r11", "memory");
    return r;
}
static inline long sys3(long n, long a1, long a2, long a3) {
    long r;
    __asm__ volatile("syscall"
        : "=a"(r)
        : "a"(n), "D"(a1), "S"(a2), "d"(a3)
        : "rcx", "r11", "memory");
    return r;
}
static inline long sys4(long n, long a1, long a2, long a3, long a4) {
    long r;
    register long r10 __asm__("r10") = a4;
    __asm__ volatile("syscall"
        : "=a"(r)
        : "a"(n), "D"(a1), "S"(a2), "d"(a3), "r"(r10)
        : "rcx", "r11", "memory");
    return r;
}

static void w(const char *s) {
    long n = 0;
    while (s[n]) n++;
    sys3(SYS_write, 1, (long)s, n);
}

void _start(void) {
    static const char banner[]   = "\n== shitix init: starting interactive shell ==\n";
    static const char respawn[]  = "\n== shell exited, respawning... ==\n";
    static const char execfail[] = "init: exec /bin/bash and /bin/sh failed\n";
    static const char forkfail[] = "init: fork failed\n";

    /* GNU LFS 用 bash；busybox 镜像没有 bash，回退到 /bin/sh。
     * 用 --norc 跳过 ~/.bashrc：其命令替换($(dircolors)、$(cat ...))依赖管道
     * EOF，而内核管道实现还不完整，会让交互启动卡住。 */
    static char *const bash_argv[] = { (char *)"/bin/bash", (char *)"--norc", (char *)"-i", (char *)0 };
    static char *const sh_argv[] = { (char *)"/bin/sh", (char *)"-i", (char *)0 };
    static char *const sh_envp[] = {
        (char *)"PATH=/bin:/sbin:/usr/bin:/usr/sbin",
        (char *)"HOME=/root",
        (char *)"TERM=linux",
        (char *)"PS1=shitix$ ",
        (char *)0,
    };

    w(banner);

    /* 成为会话首进程（session == pgrp == pid），并把 fd0 设成控制终端、
     * 前台进程组指向自己，这样子 shell 启动时就是前台（否则 busybox ash
     * 会因 tcgetpgrp 拿不到前台而给自己发 SIGTTIN 停死）。 */
    sys0(SYS_setsid);
    sys3(SYS_ioctl, 0, TIOCSCTTY, 0);

    for (;;) {
        long pid = sys0(SYS_fork);
        if (pid < 0) {
            w(forkfail);
            sys1(SYS_exit, 1);
        }
        if (pid == 0) {
            /* 子进程：先试 GNU bash，再回退 /bin/sh。成功则不返回。 */
            sys3(SYS_execve, (long)bash_argv[0], (long)bash_argv, (long)sh_envp);
            sys3(SYS_execve, (long)sh_argv[0], (long)sh_argv, (long)sh_envp);
            w(execfail);
            sys1(SYS_exit, 127);
        }

        /* 父进程（PID 1）：回收 shell；shell 退出后重新拉起，init 永不退出。 */
        int status = 0;
        sys4(SYS_wait4, pid, (long)&status, 0, 0);

        /* 顺带回收已变成孤儿（zombie）的后台任务，避免长期会话累积僵尸。 */
        for (;;) {
            long r = sys4(SYS_wait4, -1, (long)&status, WNOHANG, 0);
            if (r <= 0) break;
        }

        w(respawn);
    }
}
