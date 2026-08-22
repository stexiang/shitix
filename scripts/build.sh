#!/usr/bin/env bash
#
# 构建 shitix：bootsect + setup + system(head.S + Rust) -> 可引导磁盘镜像
#
# 布局（与 linux-1.0.9 的 build.c 产出的 zImage 思路一致）：
#   LBA 0            : bootsect (512B, 尾部 0xAA55)
#   LBA 1..4         : setup    (4 个扇区)
#   LBA 5..          : system   (head.S + Rust 内核, 纯二进制)
#
# 用法:
#   scripts/build.sh                        debug 构建（无额外 feature）
#   scripts/build.sh --release              release 构建
#   scripts/build.sh --features extra-drivers   debug + extra-drivers
#   scripts/build.sh --release --features extra-drivers  release + extra-drivers
#
# 兼容旧环境变量:
#   PROFILE=release scripts/build.sh          等价于 --release
#   FEATURES=extra-drivers scripts/build.sh   等价于 --features extra-drivers
#   如果同时传了参数和环境变量，参数优先。
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# ---- 解析命令行参数 ----
RELEASE=false
FEATURES=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release|-r)
            RELEASE=true
            shift
            ;;
        --features|-f)
            FEATURES="$2"
            shift 2
            ;;
        --features=*)
            FEATURES="${1#*=}"
            shift
            ;;
        *)
            echo "用法: $0 [--release|-r] [--features|-f <FEATURES>]" >&2
            exit 1
            ;;
    esac
done

# 环境变量兜底（参数优先）
PROFILE="${PROFILE:-debug}"
if $RELEASE; then
    PROFILE="release"
elif [[ "$PROFILE" != "release" ]]; then
    PROFILE="debug"
fi

TARGET="x86_64-unknown-none"
OUT="target/boot"
CARGO_OUT="target/$TARGET/$PROFILE/libshitix.a"
SETUPSECS=4

CARGO_FLAGS=(--target "$TARGET")
[[ "$PROFILE" == "release" ]] && CARGO_FLAGS+=(--release)
if [[ -n "$FEATURES" ]]; then
    CARGO_FLAGS+=(--features "$FEATURES")
    # 给用户一个明确提示：启用了哪些 feature
    echo "[build] features: $FEATURES"
fi

step() { printf '\033[1;34m[build]\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m[error]\033[0m %s\n' "$*" >&2; exit 1; }

for t in cargo as ld objcopy; do
    command -v "$t" >/dev/null || die "缺少工具: $t"
done

mkdir -p "$OUT"

step "汇编 AP 蹦床 (SMP 启动用)"
# 必须先于 cargo build：lib.rs 用 include_bytes! 把蹦床二进制嵌进内核
as --64 -o "$OUT/ap_trampoline.o" boot/ap_trampoline.S
ld -m elf_x86_64 -Ttext 0x2000 --oformat=binary \
    -o "$OUT/ap_trampoline.bin" "$OUT/ap_trampoline.o"
tramp_size=$(stat -c%s "$OUT/ap_trampoline.bin")
[[ "$tramp_size" -le 3840 ]] || die "AP 蹦床超出了信箱前的 3840 字节 ($tramp_size)"

step "编译 Rust 内核 ($PROFILE)"
# snap 版 cargo/rustc 走 snap-confine，而 snap-confine 的 AppArmor profile
# 不允许 mmap /usr/local/**，于是 /etc/ld.so.preload 里的宿主机统计库加载失败，
# ld.so 会打一行 "cannot be preloaded ... ignored"。纯噪音，滤掉但保留其余 stderr。
cargo build "${CARGO_FLAGS[@]}" 2> >(grep -v 'ld\.so\.preload' >&2 || true)
[[ -f "$CARGO_OUT" ]] || die "找不到 $CARGO_OUT"

step "汇编 bootsect / setup / head"
as --32 -o "$OUT/bootsect.o" boot/bootsect.S
as --32 -o "$OUT/setup.o"    boot/setup.S
as --64 -o "$OUT/head.o"     boot/head.S
as --64 -o "$OUT/entry.o"    boot/entry.S

step "链接"
ld -m elf_i386 -T boot/bootsect.ld -o "$OUT/bootsect.elf" "$OUT/bootsect.o"
ld -m elf_i386 -T boot/setup.ld    -o "$OUT/setup.elf"    "$OUT/setup.o"
ld -m elf_x86_64 -n -T boot/kernel.ld -o "$OUT/system.elf" \
    "$OUT/head.o" "$OUT/entry.o" "$CARGO_OUT"

objcopy -O binary --set-section-flags .bss=alloc,load,contents \
    "$OUT/bootsect.elf" "$OUT/bootsect.bin"
objcopy -O binary --set-section-flags .bss=alloc,load,contents \
    "$OUT/setup.elf" "$OUT/setup.bin"
objcopy -O binary -R .bss "$OUT/system.elf" "$OUT/system.bin"

# --- 校验尺寸 ---
bs_size=$(stat -c%s "$OUT/bootsect.bin")
[[ "$bs_size" -eq 512 ]] || die "bootsect 必须是 512 字节，实际 $bs_size"
if [[ "$(od -An -tx1 -j510 -N2 "$OUT/bootsect.bin" | tr -d ' \n')" != "55aa" ]]; then
    die "bootsect 缺少 0xAA55 引导标记"
fi

setup_size=$(stat -c%s "$OUT/setup.bin")
max_setup=$((SETUPSECS * 512))
[[ "$setup_size" -le "$max_setup" ]] || die "setup 超出 $SETUPSECS 个扇区 ($setup_size > $max_setup)"

sys_size=$(stat -c%s "$OUT/system.bin")
step "bootsect=${bs_size}B setup=${setup_size}B system=${sys_size}B"

# --- 组装镜像 ---
step "生成磁盘镜像"
# setup 补齐到固定的 SETUPSECS 个扇区，保证 system 的起始 LBA 可预测
cp "$OUT/setup.bin" "$OUT/setup.pad"
truncate -s "$max_setup" "$OUT/setup.pad"

IMG="$OUT/shitix.img"
cat "$OUT/bootsect.bin" "$OUT/setup.pad" "$OUT/system.bin" > "$IMG"

# 把 system 的大小（16 字节 clicks）写进 bootsect 的 syssize 字段(偏移 500)
clicks=$(( (sys_size + 15) / 16 ))
[[ "$clicks" -lt 65536 ]] || die "system 太大 ($sys_size 字节)"
python3 - "$IMG" "$clicks" <<'PY'
import sys, struct
img, clicks = sys.argv[1], int(sys.argv[2])
with open(img, "r+b") as f:
    f.seek(500)
    f.write(struct.pack("<H", clicks))
PY

# 对齐到 1MB，QEMU 才愿意当硬盘用
size=$(stat -c%s "$IMG")
aligned=$(( ((size + 1048575) / 1048576) * 1048576 ))
truncate -s "$aligned" "$IMG"

step "完成: $IMG ($(stat -c%s "$IMG") 字节, syssize=$clicks clicks)"
