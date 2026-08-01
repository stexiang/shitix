#!/usr/bin/env bash
#
# 一键编译 + 在 QEMU 中启动 + 校验启动结果
#
# 用法:
#   scripts/test.sh              构建并跑自动化启动测试（无窗口，检查串口输出）
#   scripts/test.sh run          构建并交互式运行（显示 QEMU 窗口/曲线图形）
#   scripts/test.sh debug        构建并挂起等待 GDB (localhost:1234)
#   PROFILE=release scripts/test.sh   用 release 构建
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODE="${1:-test}"
IMG="target/boot/shitix.img"
LOG="target/boot/serial.log"
TIMEOUT="${TIMEOUT:-20}"
QEMU="qemu-system-x86_64"
# 成功标记，由 src/lib.rs 在启动末尾写到串口
OK_MARK="SHITIX_BOOT_OK"

info() { printf '\033[1;34m[test]\033[0m %s\n' "$*"; }
pass() { printf '\033[1;32m[PASS]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[FAIL]\033[0m %s\n' "$*" >&2; exit 1; }

command -v "$QEMU" >/dev/null || fail "缺少 $QEMU"

info "构建"
bash scripts/build.sh

QEMU_BASE=(
    -drive "format=raw,file=$IMG,if=ide"
    -m 256M
    -no-reboot
    -no-shutdown
)

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
