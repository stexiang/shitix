//! C 风格字符串与内存块操作。对应 linux-1.0.9 的 `include/linux/string.h`
//! （实现全在头文件里的 `extern inline`，`lib/string.c` 只是把它们实例化一份）。
//!
//! 原版每个函数都是手写的 386 串指令（`lodsb`/`stosb`/`repne scasb`…）。
//! 这里改成普通 Rust：语义逐条对齐原版，但让 LLVM 自己选指令。所有函数都在
//! `*const u8` / `*mut u8` 上工作、以 NUL 结尾，因为内核后面要处理的就是
//! C 字符串（execve 的 argv、文件名、/proc 输出…），不是 `&str`。
//!
//! 注意：这里 **不** 用 `#[unsafe(no_mangle)]` 导出 `memcpy` 等符号。
//! `x86_64-unknown-none` 的 `compiler_builtins` 已经带 `mem` 特性提供了它们，
//! 重复导出会在链接期撞符号。需要编译器内建版本时直接用
//! `core::ptr::copy_nonoverlapping` / `write_bytes`。

// ---- 长度与拷贝 ----

/// 字符串长度，不含结尾 NUL。对应原版 `strlen()`。
///
/// # Safety
/// `s` 必须指向一个以 NUL 结尾、期间可读的字节序列。
pub unsafe fn strlen(s: *const u8) -> usize {
    let mut n = 0;
    // SAFETY: 由调用者保证 s 一路可读直到 NUL。
    while unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    n
}

/// 最多看 `count` 字节的字符串长度。原版没有这个函数（1.0.9 的
/// `string.h` 里确实缺 `strnlen`），但边界检查场景经常要用，补上。
///
/// # Safety
/// `s` 必须有 `count` 字节可读，或在此之前出现 NUL。
pub unsafe fn strnlen(s: *const u8, count: usize) -> usize {
    let mut n = 0;
    // SAFETY: 循环条件保证不越过 count，调用者保证这段可读。
    while n < count && unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    n
}

/// 拷贝含结尾 NUL 的字符串，返回 `dest`。对应原版 `strcpy()`。
///
/// # Safety
/// `src` 以 NUL 结尾；`dest` 至少有 `strlen(src) + 1` 字节可写；两块不重叠。
pub unsafe fn strcpy(dest: *mut u8, src: *const u8) -> *mut u8 {
    let mut i = 0;
    loop {
        // SAFETY: 调用者保证 src 可读到 NUL、dest 有足够空间。
        let c = unsafe { *src.add(i) };
        // SAFETY: 同上。
        unsafe { *dest.add(i) = c };
        if c == 0 {
            break;
        }
        i += 1;
    }
    dest
}

/// 拷贝至多 `count` 字节，不足部分用 NUL 补齐（源比 count 长时**不加**
/// 结尾 NUL，与 C 语义一致）。对应原版 `strncpy()`。
///
/// # Safety
/// `dest` 有 `count` 字节可写；`src` 可读到 NUL 或至少 `count` 字节；两块不重叠。
pub unsafe fn strncpy(dest: *mut u8, src: *const u8, count: usize) -> *mut u8 {
    let mut i = 0;
    while i < count {
        // SAFETY: i < count，调用者保证这段范围两侧都合法。
        let c = unsafe { *src.add(i) };
        // SAFETY: 同上。
        unsafe { *dest.add(i) = c };
        if c == 0 {
            break;
        }
        i += 1;
    }
    // 原版用 `rep stosb` 把剩下的填 0
    while i < count {
        // SAFETY: i < count，dest 这段可写。
        unsafe { *dest.add(i) = 0 };
        i += 1;
    }
    dest
}

/// 把 `src` 接到 `dest` 末尾。对应原版 `strcat()`。
///
/// # Safety
/// 两者都以 NUL 结尾；`dest` 有 `strlen(dest) + strlen(src) + 1` 字节可写；不重叠。
pub unsafe fn strcat(dest: *mut u8, src: *const u8) -> *mut u8 {
    // SAFETY: 契约保证 dest 是合法 C 字符串。
    let end = unsafe { strlen(dest) };
    // SAFETY: dest+end 指向原结尾 NUL，后面空间由调用者保证。
    unsafe { strcpy(dest.add(end), src) };
    dest
}

/// 最多接 `count` 字节并补上结尾 NUL。对应原版 `strncat()`。
///
/// # Safety
/// 同 [`strcat`]，且 `dest` 尾部至少有 `count + 1` 字节可写。
pub unsafe fn strncat(dest: *mut u8, src: *const u8, count: usize) -> *mut u8 {
    // SAFETY: 契约保证 dest 是合法 C 字符串。
    let end = unsafe { strlen(dest) };
    let mut i = 0;
    while i < count {
        // SAFETY: 调用者保证 src 可读、dest 尾部有 count+1 字节空间。
        let c = unsafe { *src.add(i) };
        if c == 0 {
            break;
        }
        // SAFETY: 同上。
        unsafe { *dest.add(end + i) = c };
        i += 1;
    }
    // SAFETY: 上面最多写到 end+count-1，这里写 end+i <= end+count，仍在契约范围内。
    unsafe { *dest.add(end + i) = 0 };
    dest
}

// ---- 比较 ----

/// 按无符号字节序比较，返回负 / 0 / 正。对应原版 `strcmp()`。
///
/// 原版返回值被规约成 -1/0/1（`movl $1,%eax` + `negl`），这里照做。
///
/// # Safety
/// 两个指针都必须指向以 NUL 结尾的可读字节序列。
pub unsafe fn strcmp(cs: *const u8, ct: *const u8) -> i32 {
    let mut i = 0;
    loop {
        // SAFETY: 调用者保证两侧都可读到 NUL；一旦有一侧为 0 就退出。
        let (a, b) = unsafe { (*cs.add(i), *ct.add(i)) };
        if a != b {
            return if a < b { -1 } else { 1 };
        }
        if a == 0 {
            return 0;
        }
        i += 1;
    }
}

/// 最多比较 `count` 字节。对应原版 `strncmp()`。
///
/// # Safety
/// 两侧各有 `count` 字节可读，或在此之前出现 NUL。
pub unsafe fn strncmp(cs: *const u8, ct: *const u8, count: usize) -> i32 {
    let mut i = 0;
    while i < count {
        // SAFETY: i < count，调用者保证这段可读。
        let (a, b) = unsafe { (*cs.add(i), *ct.add(i)) };
        if a != b {
            return if a < b { -1 } else { 1 };
        }
        if a == 0 {
            return 0;
        }
        i += 1;
    }
    0
}

// ---- 查找 ----

/// 找第一个 `c`（`c == 0` 时返回结尾 NUL 的位置）。对应原版 `strchr()`。
///
/// # Safety
/// `s` 必须指向以 NUL 结尾的可读字节序列。
pub unsafe fn strchr(s: *const u8, c: u8) -> *const u8 {
    let mut i = 0;
    loop {
        // SAFETY: 调用者保证 s 可读到 NUL。
        let ch = unsafe { *s.add(i) };
        if ch == c {
            // SAFETY: i 仍在 s 的有效范围内。
            return unsafe { s.add(i) };
        }
        if ch == 0 {
            return core::ptr::null();
        }
        i += 1;
    }
}

/// 找最后一个 `c`。对应原版 `strrchr()`。
///
/// # Safety
/// 同 [`strchr`]。
pub unsafe fn strrchr(s: *const u8, c: u8) -> *const u8 {
    let mut found = core::ptr::null();
    let mut i = 0;
    loop {
        // SAFETY: 调用者保证 s 可读到 NUL。
        let ch = unsafe { *s.add(i) };
        if ch == c {
            // SAFETY: i 在有效范围内。
            found = unsafe { s.add(i) };
        }
        if ch == 0 {
            return found;
        }
        i += 1;
    }
}

/// `cs` 开头连续属于 `ct` 的字符个数。对应原版 `strspn()`。
///
/// # Safety
/// 两个指针都指向以 NUL 结尾的可读字节序列。
pub unsafe fn strspn(cs: *const u8, ct: *const u8) -> usize {
    let mut n = 0;
    loop {
        // SAFETY: 调用者保证 cs 可读到 NUL。
        let c = unsafe { *cs.add(n) };
        // SAFETY: 契约保证 ct 是合法 C 字符串。
        if c == 0 || unsafe { strchr(ct, c) }.is_null() {
            return n;
        }
        n += 1;
    }
}

/// `cs` 开头连续**不**属于 `ct` 的字符个数。对应原版 `strcspn()`。
///
/// # Safety
/// 同 [`strspn`]。
pub unsafe fn strcspn(cs: *const u8, ct: *const u8) -> usize {
    let mut n = 0;
    loop {
        // SAFETY: 调用者保证 cs 可读到 NUL。
        let c = unsafe { *cs.add(n) };
        if c == 0 {
            return n;
        }
        // SAFETY: 契约保证 ct 是合法 C 字符串。注意 strchr(ct, 0) 会命中结尾
        // NUL，所以上面必须先判 c == 0，否则空集也会被当成命中。
        if !unsafe { strchr(ct, c) }.is_null() {
            return n;
        }
        n += 1;
    }
}

/// 找 `cs` 里第一个属于 `ct` 的字符。对应原版 `strpbrk()`。
///
/// # Safety
/// 同 [`strspn`]。
pub unsafe fn strpbrk(cs: *const u8, ct: *const u8) -> *const u8 {
    // SAFETY: 契约直接转交。
    let n = unsafe { strcspn(cs, ct) };
    // SAFETY: n <= strlen(cs)，加完仍在有效范围内。
    if unsafe { *cs.add(n) } == 0 {
        core::ptr::null()
    } else {
        // SAFETY: 同上。
        unsafe { cs.add(n) }
    }
}

/// 子串查找。对应原版 `strstr()`；空 `ct` 返回 `cs`（与原版一致）。
///
/// # Safety
/// 同 [`strspn`]。
pub unsafe fn strstr(cs: *const u8, ct: *const u8) -> *const u8 {
    // SAFETY: 契约保证 ct 是合法 C 字符串。
    let need = unsafe { strlen(ct) };
    if need == 0 {
        return cs;
    }
    let mut i = 0;
    loop {
        // SAFETY: 调用者保证 cs 可读到 NUL；下面的 strncmp 最多多读 need 字节，
        // 但只要前缀不匹配就会在 NUL 处停下，不会越界。
        if unsafe { *cs.add(i) } == 0 {
            return core::ptr::null();
        }
        // SAFETY: 同上。
        if unsafe { strncmp(cs.add(i), ct, need) } == 0 {
            // SAFETY: i 在有效范围内。
            return unsafe { cs.add(i) };
        }
        i += 1;
    }
}

// ---- 分词 ----

/// 原版的全局 `___strtok`，保存 [`strtok`] 的下一次起点。
static mut STRTOK_STATE: *mut u8 = core::ptr::null_mut();

/// 按 `ct` 里的任一字符切分 `s`，第二次起传空指针继续。
/// 对应原版 `strtok()`（含那个全局 `___strtok`）。
///
/// 和原版一样不可重入：会就地把分隔符改写成 NUL，且共用一份全局状态。
///
/// # Safety
/// `s` 为空指针时，必须与上一次同一个字符串的 `strtok` 调用配对；
/// 非空时必须指向以 NUL 结尾的**可写**缓冲区，且该缓冲区在整轮分词期间存活。
/// `ct` 指向以 NUL 结尾的可读字节序列。调用不能与中断上下文并发。
pub unsafe fn strtok(s: *mut u8, ct: *const u8) -> *mut u8 {
    let state = core::ptr::addr_of_mut!(STRTOK_STATE);
    // SAFETY: 单线程 + 契约要求不与中断并发，独占访问该全局。
    let mut p = if s.is_null() { unsafe { *state } } else { s };
    if p.is_null() {
        return core::ptr::null_mut();
    }

    // 跳过开头的分隔符
    // SAFETY: 契约保证 p 是合法 C 字符串、ct 合法。
    p = unsafe { p.add(strspn(p, ct)) };
    // SAFETY: p 仍在缓冲区内。
    if unsafe { *p } == 0 {
        // SAFETY: 独占访问该全局。
        unsafe { *state = core::ptr::null_mut() };
        return core::ptr::null_mut();
    }

    // 找 token 的结尾
    // SAFETY: 同上。
    let end = unsafe { p.add(strcspn(p, ct)) };
    // SAFETY: end 落在缓冲区内（最坏是结尾 NUL 处）。
    if unsafe { *end } != 0 {
        // 就地截断，下次从分隔符之后继续
        // SAFETY: 契约要求缓冲区可写。
        unsafe { *end = 0 };
        // SAFETY: end+1 仍在缓冲区内（end 处原本是分隔符，不是结尾）。
        unsafe { *state = end.add(1) };
    } else {
        // SAFETY: 独占访问该全局。
        unsafe { *state = core::ptr::null_mut() };
    }
    p
}

// ---- 内存块 ----

/// 拷贝 `n` 字节，两块**不得**重叠。对应原版 `memcpy()`。
///
/// # Safety
/// `to` 有 `n` 字节可写，`from` 有 `n` 字节可读，两块不重叠。
#[inline]
pub unsafe fn memcpy(to: *mut u8, from: *const u8, n: usize) -> *mut u8 {
    // SAFETY: 契约与 copy_nonoverlapping 的要求逐条对应。
    unsafe { core::ptr::copy_nonoverlapping(from, to, n) };
    to
}

/// 拷贝 `n` 字节，允许重叠。对应原版 `memmove()`。
///
/// # Safety
/// `dest` 有 `n` 字节可写，`src` 有 `n` 字节可读。
#[inline]
pub unsafe fn memmove(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    // SAFETY: 契约与 ptr::copy 的要求逐条对应（它自己处理重叠方向）。
    unsafe { core::ptr::copy(src, dest, n) };
    dest
}

/// 用 `c` 填充 `n` 字节。对应原版 `memset()`。
///
/// # Safety
/// `s` 有 `n` 字节可写。
#[inline]
pub unsafe fn memset(s: *mut u8, c: u8, n: usize) -> *mut u8 {
    // SAFETY: 契约与 write_bytes 的要求逐条对应。
    unsafe { core::ptr::write_bytes(s, c, n) };
    s
}

/// 按无符号字节序比较 `count` 字节。对应原版 `memcmp()`。
///
/// # Safety
/// 两侧各有 `count` 字节可读。
pub unsafe fn memcmp(cs: *const u8, ct: *const u8, count: usize) -> i32 {
    let mut i = 0;
    while i < count {
        // SAFETY: i < count，调用者保证这段可读。
        let (a, b) = unsafe { (*cs.add(i), *ct.add(i)) };
        if a != b {
            return if a < b { -1 } else { 1 };
        }
        i += 1;
    }
    0
}

/// 在前 `count` 字节里找 `c`。对应原版 `memchr()`。
///
/// # Safety
/// `cs` 有 `count` 字节可读。
pub unsafe fn memchr(cs: *const u8, c: u8, count: usize) -> *const u8 {
    let mut i = 0;
    while i < count {
        // SAFETY: i < count，调用者保证这段可读。
        if unsafe { *cs.add(i) } == c {
            // SAFETY: i < count，仍在有效范围内。
            return unsafe { cs.add(i) };
        }
        i += 1;
    }
    core::ptr::null()
}

// ---- Rust 侧便利封装 ----
//
// 原版没有对应物：C 里到处直接用裸指针，Rust 里能包成切片就包，
// 让调用方少写 unsafe。

/// 把一个 C 字符串借成字节切片（不含结尾 NUL）。
///
/// # Safety
/// `s` 必须指向以 NUL 结尾的可读字节序列，且在 `'a` 期间不被改动。
pub unsafe fn c_str_bytes<'a>(s: *const u8) -> &'a [u8] {
    // SAFETY: 契约保证能安全求长度。
    let n = unsafe { strlen(s) };
    // SAFETY: s..s+n 连续可读、生命周期内不变，且 n 远小于 isize::MAX。
    unsafe { core::slice::from_raw_parts(s, n) }
}

/// 把一个 C 字符串借成 `&str`，非 UTF-8 时返回 `None`。
///
/// # Safety
/// 同 [`c_str_bytes`]。
pub unsafe fn c_str<'a>(s: *const u8) -> Option<&'a str> {
    // SAFETY: 契约直接转交。
    core::str::from_utf8(unsafe { c_str_bytes(s) }).ok()
}
