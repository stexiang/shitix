//! 错误码。对应 linux-1.0.9 的 `include/linux/errno.h`（表体）+
//! `lib/errno.c`（那个全局 `int errno`）。
//!
//! 内核里的约定是「返回负的 errno」，所以这里的常量给成 `i32` 正值，
//! 调用方写 `-EINVAL`，和原版 C 代码的写法一致。用户态那个全局 `errno`
//! 不移植：它属于 libc（`lib/errno.c` 编出来给内核里链接的 init 用），
//! 内核自身从不读它。

/// 系统调用返回类型：`Ok(值)` 或 `Err(errno)`（正值）。
/// 原版没有这个（C 里靠负返回值），Rust 侧包一层便于用 `?`。
pub type KResult<T> = Result<T, i32>;

/// 把「负 errno / 非负结果」的 C 风格返回值转成 [`KResult`]。
#[inline]
pub fn from_raw(ret: i64) -> KResult<i64> {
    if ret < 0 { Err(-ret as i32) } else { Ok(ret) }
}

/// 把 [`KResult`] 转回 C 风格的负 errno，用于系统调用返回路径。
#[inline]
pub fn to_raw(r: KResult<i64>) -> i64 {
    match r {
        Ok(v) => v,
        Err(e) => -(e as i64),
    }
}

macro_rules! errnos {
    ($($(#[$doc:meta])* $name:ident = $val:expr, $desc:literal;)*) => {
        $(
            $(#[$doc])*
            #[doc = $desc]
            pub const $name: i32 = $val;
        )*

        /// errno 的英文描述，未知值返回 `"Unknown error"`。
        /// 原版内核没有这张表（`strerror` 在 libc 里），但打印错误时很有用。
        pub fn strerror(e: i32) -> &'static str {
            let e = if e < 0 { -e } else { e };
            match e {
                $($val => $desc,)*
                _ => "Unknown error",
            }
        }
    };
}

errnos! {
    EPERM = 1, "Operation not permitted";
    ENOENT = 2, "No such file or directory";
    ESRCH = 3, "No such process";
    EINTR = 4, "Interrupted system call";
    EIO = 5, "I/O error";
    ENXIO = 6, "No such device or address";
    E2BIG = 7, "Arg list too long";
    ENOEXEC = 8, "Exec format error";
    EBADF = 9, "Bad file number";
    ECHILD = 10, "No child processes";
    EAGAIN = 11, "Try again";
    ENOMEM = 12, "Out of memory";
    EACCES = 13, "Permission denied";
    EFAULT = 14, "Bad address";
    ENOTBLK = 15, "Block device required";
    EBUSY = 16, "Device or resource busy";
    EEXIST = 17, "File exists";
    EXDEV = 18, "Cross-device link";
    ENODEV = 19, "No such device";
    ENOTDIR = 20, "Not a directory";
    EISDIR = 21, "Is a directory";
    EINVAL = 22, "Invalid argument";
    ENFILE = 23, "File table overflow";
    EMFILE = 24, "Too many open files";
    ENOTTY = 25, "Not a typewriter";
    ETXTBSY = 26, "Text file busy";
    EFBIG = 27, "File too large";
    ENOSPC = 28, "No space left on device";
    ESPIPE = 29, "Illegal seek";
    EROFS = 30, "Read-only file system";
    EMLINK = 31, "Too many links";
    EPIPE = 32, "Broken pipe";
    EDOM = 33, "Math argument out of domain of func";
    ERANGE = 34, "Math result not representable";
    EDEADLK = 35, "Resource deadlock would occur";
    ENAMETOOLONG = 36, "File name too long";
    ENOLCK = 37, "No record locks available";
    ENOSYS = 38, "Function not implemented";
    ENOTEMPTY = 39, "Directory not empty";
    ELOOP = 40, "Too many symbolic links encountered";
    ENOMSG = 42, "No message of desired type";
    EIDRM = 43, "Identifier removed";
    ECHRNG = 44, "Channel number out of range";
    EL2NSYNC = 45, "Level 2 not synchronized";
    EL3HLT = 46, "Level 3 halted";
    EL3RST = 47, "Level 3 reset";
    ELNRNG = 48, "Link number out of range";
    EUNATCH = 49, "Protocol driver not attached";
    ENOCSI = 50, "No CSI structure available";
    EL2HLT = 51, "Level 2 halted";
    EBADE = 52, "Invalid exchange";
    EBADR = 53, "Invalid request descriptor";
    EXFULL = 54, "Exchange full";
    ENOANO = 55, "No anode";
    EBADRQC = 56, "Invalid request code";
    EBADSLT = 57, "Invalid slot";
    EDEADLOCK = 58, "File locking deadlock error";
    EBFONT = 59, "Bad font file format";
    ENOSTR = 60, "Device not a stream";
    ENODATA = 61, "No data available";
    ETIME = 62, "Timer expired";
    ENOSR = 63, "Out of streams resources";
    ENONET = 64, "Machine is not on the network";
    ENOPKG = 65, "Package not installed";
    EREMOTE = 66, "Object is remote";
    ENOLINK = 67, "Link has been severed";
    EADV = 68, "Advertise error";
    ESRMNT = 69, "Srmount error";
    ECOMM = 70, "Communication error on send";
    EPROTO = 71, "Protocol error";
    EMULTIHOP = 72, "Multihop attempted";
    EDOTDOT = 73, "RFS specific error";
    EBADMSG = 74, "Not a data message";
    EOVERFLOW = 75, "Value too large for defined data type";
    ENOTUNIQ = 76, "Name not unique on network";
    EBADFD = 77, "File descriptor in bad state";
    EREMCHG = 78, "Remote address changed";
    ELIBACC = 79, "Can not access a needed shared library";
    ELIBBAD = 80, "Accessing a corrupted shared library";
    ELIBSCN = 81, ".lib section in a.out corrupted";
    ELIBMAX = 82, "Attempting to link in too many shared libraries";
    ELIBEXEC = 83, "Cannot exec a shared library directly";
    EILSEQ = 84, "Illegal byte sequence";
    ERESTART = 85, "Interrupted system call should be restarted";
    ESTRPIPE = 86, "Streams pipe error";
    EUSERS = 87, "Too many users";
    ENOTSOCK = 88, "Socket operation on non-socket";
    EDESTADDRREQ = 89, "Destination address required";
    EMSGSIZE = 90, "Message too long";
    EPROTOTYPE = 91, "Protocol wrong type for socket";
    ENOPROTOOPT = 92, "Protocol not available";
    EPROTONOSUPPORT = 93, "Protocol not supported";
    ESOCKTNOSUPPORT = 94, "Socket type not supported";
    EOPNOTSUPP = 95, "Operation not supported on transport endpoint";
    EPFNOSUPPORT = 96, "Protocol family not supported";
    EAFNOSUPPORT = 97, "Address family not supported by protocol";
    EADDRINUSE = 98, "Address already in use";
    EADDRNOTAVAIL = 99, "Cannot assign requested address";
    ENETDOWN = 100, "Network is down";
    ENETUNREACH = 101, "Network is unreachable";
    ENETRESET = 102, "Network dropped connection because of reset";
    ECONNABORTED = 103, "Software caused connection abort";
    ECONNRESET = 104, "Connection reset by peer";
    ENOBUFS = 105, "No buffer space available";
    EISCONN = 106, "Transport endpoint is already connected";
    ENOTCONN = 107, "Transport endpoint is not connected";
    ESHUTDOWN = 108, "Cannot send after transport endpoint shutdown";
    ETOOMANYREFS = 109, "Too many references: cannot splice";
    ETIMEDOUT = 110, "Connection timed out";
    ECONNREFUSED = 111, "Connection refused";
    EHOSTDOWN = 112, "Host is down";
    EHOSTUNREACH = 113, "No route to host";
    EALREADY = 114, "Operation already in progress";
    EINPROGRESS = 115, "Operation now in progress";
    ESTALE = 116, "Stale NFS file handle";
    EUCLEAN = 117, "Structure needs cleaning";
    ENOTNAM = 118, "Not a XENIX named type file";
    ENAVAIL = 119, "No XENIX semaphores available";
    EISNAM = 120, "Is a named type file";
    EREMOTEIO = 121, "Remote I/O error";
    EDQUOT = 122, "Quota exceeded";
    // 下面三个用户态永远看不到，只在内核内部流转（原版注释如此）
    ERESTARTSYS = 512, "Restart syscall";
    ERESTARTNOINTR = 513, "Restart syscall (no EINTR)";
    ERESTARTNOHAND = 514, "Restart syscall if no handler";
}

/// `EAGAIN` 的别名，对应原版 `#define EWOULDBLOCK EAGAIN`。
pub const EWOULDBLOCK: i32 = EAGAIN;

/// `struct utsname` 每个字段的长度（含结尾 NUL），对应 glibc 的
/// `_UTSNAME_LENGTH`（64 + 1）。
pub const utsname_len: usize = 65;
