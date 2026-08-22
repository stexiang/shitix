//! SysV 消息队列。对应原版 `ipc/msg.c` 的简化移植：
//! 静态队列表 + 每队列定长消息池（满则睡、空则睡，IPC_NOWAIT 除外）。
//!
//! - `msgget(key, flags)`：按 key 查找/创建队列
//! - `msgsnd(id, msgp, msgsz, flags)`：按 mtype 入队（msgbuf 头部是 i64 mtype）
//! - `msgrcv(id, msgp, msgsz, msgtyp, flags)`：按 msgtyp 规则出队
//! - `msgctl(id, cmd, buf)`：IPC_RMID / IPC_STAT
//!
//! msgtyp 选择规则（原版 find_msg）：
//!   0  → 队首
//!   >0 → 第一个 mtype==msgtyp 的
//!   <0 → mtype <= |msgtyp| 中最小的（同类取先入队的）

use crate::klib::errno::{EINVAL, EEXIST, ENOENT, ENOSPC, EAGAIN, EIDRM, ENOMEM, E2BIG};

/// 最大队列数
const MAX_QUEUES: usize = 16;
/// 每队列最大消息数
const MAX_MSGS: usize = 16;
/// 单条消息最大字节数（不含 mtype 头）
const MAX_MSG_SIZE: usize = 1024;

pub const IPC_CREAT: i32 = 0o1000;
pub const IPC_EXCL: i32 = 0o2000;
pub const IPC_PRIVATE: i32 = 0;
pub const IPC_NOWAIT: i32 = 0o4000;
pub const IPC_RMID: i32 = 0;
pub const IPC_STAT: i32 = 2;
/// msgrcv 的 MSG_NOERROR：超长截断而非报错
pub const MSG_NOERROR: i32 = 0o10000;

/// 一条消息
#[derive(Clone, Copy)]
struct Msg {
    used: bool,
    mtype: i64,
    len: usize,
    data: [u8; MAX_MSG_SIZE],
}

const EMPTY_MSG: Msg = Msg { used: false, mtype: 0, len: 0, data: [0; MAX_MSG_SIZE] };

/// 一个消息队列
#[derive(Clone, Copy)]
struct MsgQueue {
    used: bool,
    key: i32,
    msgs: [Msg; MAX_MSGS],
}

const EMPTY_QUEUE: MsgQueue = MsgQueue { used: false, key: 0, msgs: [EMPTY_MSG; MAX_MSGS] };

static mut QUEUES: [MsgQueue; MAX_QUEUES] = [EMPTY_QUEUE; MAX_QUEUES];

fn find_by_key(key: i32) -> Option<usize> {
    // SAFETY: 调用点在临界区内。
    unsafe {
        for (i, q) in (*core::ptr::addr_of!(QUEUES)).iter().enumerate() {
            if q.used && q.key == key && key != IPC_PRIVATE {
                return Some(i);
            }
        }
    }
    None
}

/// msgget(key, msgflg)。
pub fn sys_msgget(key: i32, msgflg: i32) -> i64 {
    // SAFETY: 关中断临界区独占队列表。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let qs = &mut *core::ptr::addr_of_mut!(QUEUES);
        let r = if let Some(idx) = find_by_key(key) {
            if msgflg & (IPC_CREAT | IPC_EXCL) == (IPC_CREAT | IPC_EXCL) {
                Err(EEXIST)
            } else {
                Ok(idx)
            }
        } else if msgflg & IPC_CREAT == 0 && key != IPC_PRIVATE {
            Err(ENOENT)
        } else {
            match qs.iter().position(|q| !q.used) {
                None => Err(ENOSPC),
                Some(idx) => {
                    qs[idx] = EMPTY_QUEUE;
                    qs[idx].used = true;
                    qs[idx].key = key;
                    Ok(idx)
                }
            }
        };
        crate::irq::restore_flags(flags);
        match r {
            Ok(idx) => idx as i64,
            Err(e) => -(e as i64),
        }
    }
}

/// msgsnd(id, msgp, msgsz, msgflg)。msgp 指向 msgbuf { mtype i64, mtext[] }。
pub fn sys_msgsnd(id: usize, msgp: *const u8, msgsz: usize, msgflg: i32) -> i64 {
    if msgp.is_null() {
        return -(EINVAL as i64);
    }
    if msgsz > MAX_MSG_SIZE {
        return -(E2BIG as i64);
    }
    // SAFETY: 用户指针读 mtype 头。
    let mtype = unsafe { (msgp as *const i64).read_volatile() };
    if mtype <= 0 {
        return -(EINVAL as i64);
    }
    loop {
        // SAFETY: 临界区试入队。
        let r = unsafe {
            let flags = crate::irq::local_irq_save();
            let qs = &mut *core::ptr::addr_of_mut!(QUEUES);
            let r = if id >= MAX_QUEUES || !qs[id].used {
                Err(EIDRM)
            } else if let Some(slot) = qs[id].msgs.iter().position(|m| !m.used) {
                let m = &mut qs[id].msgs[slot];
                m.used = true;
                m.mtype = mtype;
                m.len = msgsz;
                // SAFETY: 用户指针读消息体。
                core::ptr::copy_nonoverlapping(msgp.add(8), m.data.as_mut_ptr(), msgsz);
                Ok(())
            } else {
                Err(ENOMEM)
            };
            crate::irq::restore_flags(flags);
            r
        };
        match r {
            Ok(()) => return 0,
            Err(EIDRM) => return -(EIDRM as i64),
            Err(_) => {
                // 队列满
                if msgflg & IPC_NOWAIT != 0 {
                    return -(EAGAIN as i64);
                }
                // 睡到有人取走消息
                // SAFETY: 系统调用上下文让出。
                unsafe { crate::sched::schedule() };
            }
        }
    }
}

/// msgrcv(id, msgp, msgsz, msgtyp, msgflg)。返回拷贝出的消息体字节数。
pub fn sys_msgrcv(id: usize, msgp: *mut u8, msgsz: usize, msgtyp: i64, msgflg: i32) -> i64 {
    if msgp.is_null() {
        return -(EINVAL as i64);
    }
    loop {
        // SAFETY: 临界区按 msgtyp 规则挑出队。
        let r = unsafe {
            let flags = crate::irq::local_irq_save();
            let qs = &mut *core::ptr::addr_of_mut!(QUEUES);
            let r: Result<(i64, usize), i32> = if id >= MAX_QUEUES || !qs[id].used {
                Err(EIDRM)
            } else {
                let q = &mut qs[id];
                let pick = if msgtyp == 0 {
                    q.msgs.iter().position(|m| m.used)
                } else if msgtyp > 0 {
                    q.msgs.iter().position(|m| m.used && m.mtype == msgtyp)
                } else {
                    // 最小 mtype <= |msgtyp|
                    let want = -msgtyp;
                    let mut best: Option<usize> = None;
                    for (i, m) in q.msgs.iter().enumerate() {
                        if m.used && m.mtype <= want {
                            match best {
                                None => best = Some(i),
                                Some(b) if q.msgs[b].mtype > m.mtype => best = Some(i),
                                _ => {}
                            }
                        }
                    }
                    best
                };
                match pick {
                    None => Err(EAGAIN),
                    Some(slot) => {
                        let m = q.msgs[slot];
                        if m.len > msgsz && msgflg & MSG_NOERROR == 0 {
                            Err(E2BIG)
                        } else {
                            let n = m.len.min(msgsz);
                            // SAFETY: 用户指针写 mtype 头与消息体。
                            (msgp as *mut i64).write_volatile(m.mtype);
                            core::ptr::copy_nonoverlapping(m.data.as_ptr(), msgp.add(8), n);
                            q.msgs[slot] = EMPTY_MSG;
                            Ok((m.mtype, n))
                        }
                    }
                }
            };
            crate::irq::restore_flags(flags);
            r
        };
        match r {
            Ok((_t, n)) => return n as i64,
            Err(EIDRM) => return -(EIDRM as i64),
            Err(E2BIG) => return -(E2BIG as i64),
            Err(_) => {
                if msgflg & IPC_NOWAIT != 0 {
                    return -(ENOMSG as i64);
                }
                // 睡到有新消息
                // SAFETY: 系统调用上下文让出。
                unsafe { crate::sched::schedule() };
            }
        }
    }
}

/// ENOMSG：msgrcv 队列空且 NOWAIT
const ENOMSG: i32 = 42;

/// msgctl(id, cmd, buf)。
pub fn sys_msgctl(id: usize, cmd: i32, buf: *mut u8) -> i64 {
    match cmd {
        IPC_RMID => {
            // SAFETY: 临界区。
            unsafe {
                let flags = crate::irq::local_irq_save();
                let qs = &mut *core::ptr::addr_of_mut!(QUEUES);
                if id >= MAX_QUEUES || !qs[id].used {
                    crate::irq::restore_flags(flags);
                    return -(EINVAL as i64);
                }
                qs[id] = EMPTY_QUEUE;
                crate::irq::restore_flags(flags);
            }
            0
        }
        IPC_STAT => {
            if buf.is_null() {
                return -(EINVAL as i64);
            }
            // SAFETY: 临界区读队列，用户指针恒等映射可写。
            unsafe {
                let flags = crate::irq::local_irq_save();
                let qs = &*core::ptr::addr_of!(QUEUES);
                if id >= MAX_QUEUES || !qs[id].used {
                    crate::irq::restore_flags(flags);
                    return -(EINVAL as i64);
                }
                let key = qs[id].key;
                let qnum = qs[id].msgs.iter().filter(|m| m.used).count() as u64;
                crate::irq::restore_flags(flags);
                // x86_64 struct msqid_ds：ipc_perm 48 字节，随后
                // stime@48 rtime@56 ctime@64 msg_cbytes@72 msg_qnum@80
                // msg_qbytes@88 lspid@96 lrpid@100
                core::ptr::write_bytes(buf, 0, 104);
                (buf as *mut i32).write_volatile(key);
                (buf.add(20) as *mut u32).write_volatile(0o666);
                (buf.add(80) as *mut u64).write_volatile(qnum);
                (buf.add(88) as *mut u64).write_volatile((MAX_MSGS * MAX_MSG_SIZE) as u64);
            }
            0
        }
        _ => -(EINVAL as i64),
    }
}
