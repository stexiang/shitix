//! POSIX 消息队列（mq_open/mq_timedsend/mq_timedreceive/mq_unlink/
//! mq_notify/mq_getsetattr）。
//!
//! Linux 把这些挂在 mqueue 伪文件系统上、mqd_t 就是 fd；本树没有
//! mqueuefs，mqd_t 用「魔数 | 队列下标」编码（见 [`encode`]），不进 fd
//! 表——glibc 的 mq_close 会调 close() 得到 EBADF（无害失败），比
//! 「mqd 恰好等于 0/1/2 被 close 掉 stdin」安全得多。
//!
//! 优先级：每条消息带 prio，接收取最高 prio 中最先入队的（原版语义）。
//! 阻塞语义：满则睡到有空位、空则睡到有消息，abs_timeout 用
//! CLOCK_REALTIME 秒换算成内部 tick 判定。

use crate::klib::errno::{EINVAL, EEXIST, ENOENT, ENOSPC, EAGAIN, EBADF, EMSGSIZE, ETIMEDOUT};

/// 最大队列数
const MAX_MQS: usize = 8;
/// 队列名最大长度（含 NUL）
const MAX_NAME: usize = 64;
/// 每队列最大消息数（attr 可更小）
const MAX_MSGS: usize = 16;
/// 单条消息最大字节数（attr 可更小）
const MAX_MSG_SIZE: usize = 1024;

/// mqd_t 编码魔数：`0x4D51_0000 | idx`
const MQ_MAGIC: i64 = 0x4D51_0000;

/// open 标志位
const O_CREAT: i32 = 0o100;
const O_EXCL: i32 = 0o200;
const O_NONBLOCK: i32 = 0o4000;

/// 一条消息
#[derive(Clone, Copy)]
struct Msg {
    used: bool,
    prio: u32,
    len: usize,
    data: [u8; MAX_MSG_SIZE],
}

const EMPTY_MSG: Msg = Msg { used: false, prio: 0, len: 0, data: [0; MAX_MSG_SIZE] };

/// 一个消息队列
#[derive(Clone, Copy)]
struct Mq {
    used: bool,
    name: [u8; MAX_NAME],
    maxmsg: usize,
    msgsize: usize,
    nonblock: bool,
    msgs: [Msg; MAX_MSGS],
}

const EMPTY_MQ: Mq = Mq {
    used: false, name: [0; MAX_NAME], maxmsg: 0, msgsize: 0,
    nonblock: false, msgs: [EMPTY_MSG; MAX_MSGS],
};

static mut MQS: [Mq; MAX_MQS] = [EMPTY_MQ; MAX_MQS];

fn encode(idx: usize) -> i64 { MQ_MAGIC | idx as i64 }

fn decode(mqd: i64) -> Option<usize> {
    if mqd & !0xFFFF != MQ_MAGIC { return None; }
    let idx = (mqd & 0xFFFF) as usize;
    // SAFETY: 只读判 used。
    let used = unsafe { (*core::ptr::addr_of!(MQS))[idx.min(MAX_MQS - 1)].used };
    if idx < MAX_MQS && used { Some(idx) } else { None }
}

/// 从用户空间读 NUL 结尾的队列名，返回（写入长度，含 NUL）。
/// 失败返回 0（空名或超长）。
unsafe fn read_name(ptr: *const u8, out: &mut [u8; MAX_NAME]) -> usize {
    if ptr.is_null() { return 0; }
    for i in 0..MAX_NAME {
        // SAFETY: 用户指针，逐字节读到 NUL。
        let c = unsafe { ptr.add(i).read_volatile() };
        out[i] = c;
        if c == 0 {
            // 名字必须是 "/xxx" 形式（POSIX）
            return if out[0] == b'/' && i >= 2 { i + 1 } else { 0 };
        }
    }
    0
}

fn find_by_name(name: &[u8; MAX_NAME]) -> Option<usize> {
    // SAFETY: 调用点在临界区内。
    unsafe {
        for (i, q) in (*core::ptr::addr_of!(MQS)).iter().enumerate() {
            if q.used && &q.name == name {
                return Some(i);
            }
        }
    }
    None
}

/// mq_open(name, oflag, mode, attr)。attr 为 NULL 用默认 16/1024。
pub fn sys_mq_open(name_ptr: *const u8, oflag: i32, _mode: u32, attr_ptr: *const u64) -> i64 {
    let mut name = [0u8; MAX_NAME];
    // SAFETY: 用户指针读名字。
    if unsafe { read_name(name_ptr, &mut name) } == 0 {
        return -(ENOENT as i64);
    }
    let (mut maxmsg, mut msgsize) = (8usize, 256usize);
    if !attr_ptr.is_null() {
        // mq_attr { flags, maxmsg, msgsize, curmsgs } 四个 i64
        // SAFETY: 用户指针读 attr。
        let (m, s) = unsafe { (
            (attr_ptr.add(1) as *const i64).read_volatile(),
            (attr_ptr.add(2) as *const i64).read_volatile(),
        ) };
        if m <= 0 || s <= 0 { return -(EINVAL as i64); }
        maxmsg = (m as usize).min(MAX_MSGS);
        msgsize = (s as usize).min(MAX_MSG_SIZE);
    }
    // SAFETY: 关中断临界区独占队列表。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let qs = &mut *core::ptr::addr_of_mut!(MQS);
        let r = if let Some(idx) = find_by_name(&name) {
            if oflag & (O_CREAT | O_EXCL) == (O_CREAT | O_EXCL) {
                Err(EEXIST)
            } else {
                Ok(idx)
            }
        } else if oflag & O_CREAT == 0 {
            Err(ENOENT)
        } else {
            match qs.iter().position(|q| !q.used) {
                None => Err(ENOSPC),
                Some(idx) => {
                    qs[idx] = EMPTY_MQ;
                    qs[idx].used = true;
                    qs[idx].name = name;
                    qs[idx].maxmsg = maxmsg;
                    qs[idx].msgsize = msgsize;
                    Ok(idx)
                }
            }
        };
        if let Ok(idx) = r {
            // O_NONBLOCK 记在队列上（简化：句柄即队列，见模块头注释）
            if oflag & O_NONBLOCK != 0 { qs[idx].nonblock = true; }
        }
        crate::irq::restore_flags(flags);
        match r {
            Ok(idx) => encode(idx),
            Err(e) => -(e as i64),
        }
    }
}

/// mq_unlink(name)。
pub fn sys_mq_unlink(name_ptr: *const u8) -> i64 {
    let mut name = [0u8; MAX_NAME];
    // SAFETY: 用户指针读名字。
    if unsafe { read_name(name_ptr, &mut name) } == 0 {
        return -(ENOENT as i64);
    }
    // SAFETY: 临界区（句柄即队列，unlink 即销毁——没有引用计数可等）。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let qs = &mut *core::ptr::addr_of_mut!(MQS);
        let r = match find_by_name(&name) {
            None => Err(ENOENT),
            Some(idx) => {
                qs[idx] = EMPTY_MQ;
                Ok(())
            }
        };
        crate::irq::restore_flags(flags);
        match r {
            Ok(()) => 0,
            Err(e) => -(e as i64),
        }
    }
}

/// 把 CLOCK_REALTIME 的绝对超时 timespec 换成「还剩多少 tick」。
/// None 表示无限。超时已过期返回 Some(0)。
fn abs_to_remaining_ticks(ts: *const u64) -> Option<u64> {
    if ts.is_null() { return None; }
    // SAFETY: 用户指针读 timespec {sec, nsec}。
    let (sec, nsec) = unsafe { (
        core::ptr::read_volatile(ts),
        core::ptr::read_volatile(ts.add(1)),
    ) };
    if nsec >= 1_000_000_000 { return Some(0); }
    let now = crate::sched::current_time() as u64;
    if sec <= now { return Some(0); }
    let hz = crate::sched::task::HZ;
    Some((sec - now).saturating_mul(hz))
}

/// mq_timedsend(mqd, msg_ptr, len, prio, abs_timeout)。
pub fn sys_mq_timedsend(mqd: i64, msg_ptr: *const u8, len: usize, prio: u32, ts: *const u64) -> i64 {
    let idx = match decode(mqd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    if msg_ptr.is_null() { return -(EINVAL as i64); }
    let mut remaining = abs_to_remaining_ticks(ts);
    loop {
        // SAFETY: 临界区试入队。
        let r = unsafe {
            let flags = crate::irq::local_irq_save();
            let qs = &mut *core::ptr::addr_of_mut!(MQS);
            let q = &mut qs[idx];
            let r = if len > q.msgsize {
                Err(EMSGSIZE)
            } else {
                let cur = q.msgs.iter().filter(|m| m.used).count();
                if cur < q.maxmsg {
                    let slot = q.msgs.iter().position(|m| !m.used).unwrap();
                    let m = &mut q.msgs[slot];
                    m.used = true;
                    m.prio = prio;
                    m.len = len;
                    // SAFETY: 用户指针读消息体。
                    core::ptr::copy_nonoverlapping(msg_ptr, m.data.as_mut_ptr(), len);
                    Ok(())
                } else {
                    Err(EAGAIN)
                }
            };
            crate::irq::restore_flags(flags);
            r
        };
        match r {
            Ok(()) => return 0,
            Err(EMSGSIZE) => return -(EMSGSIZE as i64),
            Err(_) => {
                // SAFETY: 临界区读 nonblock。
                let nb = unsafe {
                    let flags = crate::irq::local_irq_save();
                    let v = (*core::ptr::addr_of!(MQS))[idx].nonblock;
                    crate::irq::restore_flags(flags);
                    v
                };
                if nb { return -(EAGAIN as i64); }
                if let Some(t) = remaining {
                    if t == 0 { return -(ETIMEDOUT as i64); }
                    remaining = Some(t - 1);
                }
                // SAFETY: 系统调用上下文让出。
                unsafe { crate::sched::schedule() };
            }
        }
    }
}

/// mq_timedreceive(mqd, msg_ptr, len, prio_ptr, abs_timeout)。返回字节数。
pub fn sys_mq_timedreceive(mqd: i64, msg_ptr: *mut u8, len: usize, prio_ptr: *mut u32, ts: *const u64) -> i64 {
    let idx = match decode(mqd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    if msg_ptr.is_null() { return -(EINVAL as i64); }
    let mut remaining = abs_to_remaining_ticks(ts);
    loop {
        // SAFETY: 临界区取最高 prio 中最先入队的消息。
        let r = unsafe {
            let flags = crate::irq::local_irq_save();
            let qs = &mut *core::ptr::addr_of_mut!(MQS);
            let q = &mut qs[idx];
            let mut best: Option<usize> = None;
            for (i, m) in q.msgs.iter().enumerate() {
                if m.used {
                    match best {
                        None => best = Some(i),
                        Some(b) if q.msgs[b].prio < m.prio => best = Some(i),
                        _ => {}
                    }
                }
            }
            let r = match best {
                None => Err(EAGAIN),
                Some(slot) => {
                    let m = q.msgs[slot];
                    if len < m.len {
                        Err(EMSGSIZE)
                    } else {
                        // SAFETY: 用户指针写消息体。
                        core::ptr::copy_nonoverlapping(m.data.as_ptr(), msg_ptr, m.len);
                        if !prio_ptr.is_null() {
                            prio_ptr.write_volatile(m.prio);
                        }
                        q.msgs[slot] = EMPTY_MSG;
                        Ok(m.len)
                    }
                }
            };
            crate::irq::restore_flags(flags);
            r
        };
        match r {
            Ok(n) => return n as i64,
            Err(EMSGSIZE) => return -(EMSGSIZE as i64),
            Err(_) => {
                // SAFETY: 临界区读 nonblock。
                let nb = unsafe {
                    let flags = crate::irq::local_irq_save();
                    let v = (*core::ptr::addr_of!(MQS))[idx].nonblock;
                    crate::irq::restore_flags(flags);
                    v
                };
                if nb { return -(EAGAIN as i64); }
                if let Some(t) = remaining {
                    if t == 0 { return -(ETIMEDOUT as i64); }
                    remaining = Some(t - 1);
                }
                // SAFETY: 系统调用上下文让出。
                unsafe { crate::sched::schedule() };
            }
        }
    }
}

/// mq_notify(mqd, sevp)：不支持异步通知投递；SIGEV_NONE 与立即
/// 注册都按成功处理（应用轮询语义不受影响）。
pub fn sys_mq_notify(mqd: i64, _sevp: u64) -> i64 {
    match decode(mqd) {
        Some(_) => 0,
        None => -(EBADF as i64),
    }
}

/// mq_getsetattr(mqd, new, old)。只有 O_NONBLOCK 可改（POSIX 语义）。
pub fn sys_mq_getsetattr(mqd: i64, new_ptr: *const u64, old_ptr: *mut u64) -> i64 {
    let idx = match decode(mqd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: 临界区读/改队列属性，用户指针恒等映射。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let qs = &mut *core::ptr::addr_of_mut!(MQS);
        let q = &mut qs[idx];
        if !old_ptr.is_null() {
            let cur = q.msgs.iter().filter(|m| m.used).count() as u64;
            let nb = if q.nonblock { O_NONBLOCK as u64 } else { 0 };
            old_ptr.write_volatile(nb);                     // flags
            old_ptr.add(1).write_volatile(q.maxmsg as u64); // maxmsg
            old_ptr.add(2).write_volatile(q.msgsize as u64);// msgsize
            old_ptr.add(3).write_volatile(cur);             // curmsgs
        }
        if !new_ptr.is_null() {
            let f = new_ptr.read_volatile();
            q.nonblock = f & (O_NONBLOCK as u64) != 0;
        }
        crate::irq::restore_flags(flags);
    }
    0
}
