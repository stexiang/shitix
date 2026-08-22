#!/usr/bin/env bash
#
# 一键编译 + 在 QEMU 中启动 + 校验启动结果
#
# 用法:
#   scripts/test.sh                    构建并跑自动化启动测试（debug，无窗口）
#   scripts/test.sh run                构建并交互式运行（显示 QEMU 窗口）
#   scripts/test.sh debug              构建并挂起等待 GDB (localhost:1234)
#   scripts/test.sh --release          用 release 构建并测试
#   scripts/test.sh --release run      用 release 构建并交互运行
#   scripts/test.sh --features extra-drivers --release   release + 完整驱动
#
# 旧用法兼容:
#   PROFILE=release scripts/test.sh    等价于 --release
#   MEM=32M scripts/test.sh            改 QEMU 内存大小（默认 256M）
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MEM="${MEM:-256M}"
IMG="target/boot/shitix.img"
LOG="target/boot/serial.log"
TIMEOUT="${TIMEOUT:-20}"
QEMU="qemu-system-x86_64"
OK_MARK="SHITIX_BOOT_OK"

# ---- 分离 build 参数与 test.sh 模式 ----
BUILD_ARGS=()
MODE="test"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release|-r)
            BUILD_ARGS+=("--release")
            shift
            ;;
        --features|-f)
            BUILD_ARGS+=("--features" "$2")
            shift 2
            ;;
        --features=*)
            BUILD_ARGS+=("${1}")
            shift
            ;;
        test|run|debug)
            MODE="$1"
            shift
            ;;
        *)
            echo "用法: $0 [--release|-r] [--features|-f <FEATURES>] [test|run|debug]" >&2
            exit 1
            ;;
    esac
done

# 环境变量兜底
if [[ -n "${PROFILE:-}" ]] && ! printf '%s\n' "${BUILD_ARGS[@]}" | grep -q '\--release'; then
    [[ "$PROFILE" == "release" ]] && BUILD_ARGS+=("--release")
fi

info() { printf '\033[1;34m[test]\033[0m %s\n' "$*"; }
pass() { printf '\033[1;32m[PASS]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[FAIL]\033[0m %s\n' "$*" >&2; exit 1; }

command -v "$QEMU" >/dev/null || fail "缺少 $QEMU"

info "构建"
bash scripts/build.sh "${BUILD_ARGS[@]}"

# SMP: 默认 4 核（AP 蹦床/INIT-SIPI-SIPI/并行求和自检都依赖多核路径），
# 可用 SMP=1 退回单核。
SMP="${SMP:-4}"
QEMU_BASE=(
    -drive "format=raw,file=$IMG,if=ide"
    -m "$MEM"
    -smp "$SMP"
    -no-reboot
    -no-shutdown
)

# ROOTIMG: pass a second IDE drive as the root filesystem (ext2)
if [[ -n "${ROOTIMG:-}" ]]; then
    [[ -f "$ROOTIMG" ]] || fail "ROOTIMG=$ROOTIMG not found"
    QEMU_BASE+=(-drive "format=raw,file=$ROOTIMG,if=ide")
    info "Root image: $ROOTIMG"
fi

case "$MODE" in
run)
    info "交互式运行（关闭窗口即退出）"
    exec "$QEMU" "${QEMU_BASE[@]}" -serial stdio
    ;;

debug)
    info "等待 GDB 连接 localhost:1234"
    info "另开一个终端: gdb -ex 'target remote :1234' target/boot/system.elf"
    exec "$QEMU" "${QEMU_BASE[@]}" -serial stdio -s -S
    ;;

test)
    info "无头启动，超时 ${TIMEOUT}s，串口日志 -> $LOG"
    rm -f "$LOG"
    set +e
    timeout "$TIMEOUT" "$QEMU" "${QEMU_BASE[@]}" \
        -display none \
        -serial "file:$LOG" \
        -monitor none
    rc=$?
    set -e
    # 内核以 hlt 空转，所以正常路径一定是被 timeout 杀掉(124)
    if [[ $rc -ne 0 && $rc -ne 124 ]]; then
        info "QEMU 退出码 $rc"
    fi

    [[ -f "$LOG" ]] || fail "没有产生串口输出，内核可能没进到 long mode"

    echo "--- 串口输出 ---"
    cat "$LOG"
    echo "----------------"

    if grep -q "SHITIX_PANIC" "$LOG"; then
        fail "内核 panic"
    fi
    if grep -q "$OK_MARK" "$LOG"; then
        pass "内核启动成功（检测到 $OK_MARK）"
    else
        fail "未检测到 $OK_MARK，启动未走完"
    fi
    ;;

*)
    fail "未知模式: $MODE（可用: test | run | debug）"
    ;;
esac
