#!/usr/bin/env python3
"""核对 src/syscall/mod.rs 的 nr 模块与官方 x86_64 调用号表是否逐项一致。

背景：早期 nr 模块按功能分组手写号，出现 GETPID=WAIT4=61 这类重号。
分发表是 `t[nr::X] = ...` 的顺序赋值，重号会**静默覆盖**，编译器零警告，
只有运行时自检才发现 getppid 实际跑的是 sys_kill（buglog bug-026）。
所以调用号必须机器核对，不能靠眼睛。

用法：
    scripts/check-syscall-nr.py [/path/to/unistd_64.h]

不给参数时自动找本机的内核头文件。退出码非 0 表示有不一致。
"""

import re
import sys
import glob

# 本树自己加的号，官方表里不该有
PRIVATE = {
    "idle",          # 1.0.9 的 sys_idle，官方 x86_64 表里 112 是 setsid，挪到私有段
    "unused",        # 自检用：保证落在表内且保证未实现
    "nr_syscalls",   # 表容量，不是调用号
    # 旧代码留下的别名，指向同一个号
    "umount", "prlimit", "setmempolicy", "getmempolicy",
}

# 拼写差异：本树名 -> 官方名（号必须一致）
SPELLING = {"sysctl": "_sysctl"}


def find_header():
    pats = [
        "/usr/src/linux-headers-*/arch/x86/include/generated/uapi/asm/unistd_64.h",
        "/usr/include/x86_64-linux-gnu/asm/unistd_64.h",
        "/usr/include/asm/unistd_64.h",
    ]
    for p in pats:
        hits = sorted(glob.glob(p))
        if hits:
            return hits[-1]
    return None


def parse_official(path):
    out = {}
    with open(path) as f:
        for line in f:
            m = re.match(r"#define __NR_([a-z0-9_]+)\s+(\d+)", line)
            if m:
                out[m.group(1)] = int(m.group(2))
    out.pop("syscalls", None)  # 表容量宏，不是调用号
    return out


def parse_mine(path):
    src = open(path).read()
    start = src.index("pub mod nr {")
    body = src[start:src.index("\n}\n", start)]
    return {
        m.group(1).lower(): int(m.group(2))
        for m in re.finditer(r"pub const ([A-Z0-9_]+): usize = (\d+);", body)
    }


def main():
    hdr = sys.argv[1] if len(sys.argv) > 1 else find_header()
    if not hdr:
        print("找不到 unistd_64.h，请显式给路径", file=sys.stderr)
        return 2
    official = parse_official(hdr)
    mine = parse_mine("src/syscall/mod.rs")
    mine_real = {n: v for n, v in mine.items() if n not in PRIVATE}

    bad = []
    for n, v in sorted(mine_real.items(), key=lambda kv: kv[1]):
        key = SPELLING.get(n, n)
        if key not in official:
            bad.append(f"  {n}={v} 官方表里没有这个名字")
        elif official[key] != v:
            bad.append(f"  {n}={v} 官方是 {official[key]}")

    rev = {SPELLING.get(n, n) for n in mine_real}
    for n, v in sorted(official.items(), key=lambda kv: kv[1]):
        if n not in rev:
            bad.append(f"  官方 {n}={v} 本树缺")

    # 重号自查：官方表本身无重号，本树也不该有
    seen = {}
    for n, v in mine_real.items():
        if v in seen:
            bad.append(f"  重号 {v}: {seen[v]} 与 {n}（分发表会静默覆盖！）")
        seen[v] = n

    print(f"header: {hdr}")
    print(f"官方 {len(official)} 个号，本树 {len(mine_real)} 个（另有 {len(mine)-len(mine_real)} 个私有/别名）")
    if bad:
        print(f"不一致 {len(bad)} 处:")
        print("\n".join(bad))
        return 1
    print("全部一致")
    return 0


if __name__ == "__main__":
    sys.exit(main())
