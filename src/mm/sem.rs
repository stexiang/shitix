//! SysV 信号量。对应原版 `ipc/sem.c` 的简化移植：
//! 静态集合表，每集合固定个数的信号量（计数型，0..SEMVMX）。
//!
//! - `semget(key, nsems, flags)`：按 key 查找/创建集合
//! - `semop(id, ops, nops)`：sem_op>0 加；<0 减（不够则睡到够）；
//!   =0 等到归零（IPC_NOWAIT 时不睡直接 EAGAIN）
//! - `semctl(id, num, cmd, arg)`：GETVAL/SETVAL/GETALL/SETALL/IPC_RMID
//! - `semtimedop`：semop + 纳秒超时
//!
//! 阻塞语义：调用者循环 `schedule()` 让出，其他任务的 semop/semctl
//! 改变计数后自然唤醒（合作式调度下没有专门的 wait queue 也能工作）。

use crate::klib::errno::{EINVAL, EEXIST, ENOENT, ENOSPC, EAGAIN, EIDRM, ERANGE};

/// 最大集合数
const MAX_SETS: usize = 16;
/// 每集合最大信号量数
const MAX_SEMS: usize = 16;
/// 信号量最大计数值（原版 SEMVMX=32767）
const SEMVMX: i32 = 32767;

pub const IPC_CREAT: i32 = 0o1000;
pub const IPC_EXCL: i32 = 0o2000;
pub const IPC_PRIVATE: i32 = 0;
/// semflg
pub const IPC_NOWAIT: i32 = 0o4000;
/// semctl 命令
pub const IPC_RMID: i32 = 0;
pub const GETVAL: i32 = 6;
pub const SETVAL: i32 = 8;
pub const GETALL: i32 = 10;
pub const SETALL: i32 = 11;

/// 一个信号量集合
#[derive(Clone, Copy)]
struct SemSet {
    used: bool,
    key: i32,
    nsems: usize,
    vals: [i32; MAX_SEMS],
}

const EMPTY_SET: SemSet = SemSet { used: false, key: 0, nsems: 0, vals: [0; MAX_SEMS] };

static mut SETS: [SemSet; MAX_SETS] = [EMPTY_SET; MAX_SETS];

fn find_by_key(key: i32) -> Option<usize> {
    // SAFETY: 调用点在临界区内。
    unsafe {
        for (i, s) in (*core::ptr::addr_of!(SETS)).iter().enumerate() {
            if s.used && s.key == key && key != IPC_PRIVATE {
                return Some(i);
            }
        }
    }
    None
}

/// semget(key, nsems, semflg)。
pub fn sys_semget(key: i32, nsems: i32, semflg: i32) -> i64 {
    if nsems < 0 || nsems as usize > MAX_SEMS {
        return -(EINVAL as i64);
    }
    // SAFETY: 关中断临界区独占集合表。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let sets = &mut *core::ptr::addr_of_mut!(SETS);
        let r = if let Some(idx) = find_by_key(key) {
            if semflg & (IPC_CREAT | IPC_EXCL) == (IPC_CREAT | IPC_EXCL) {
                Err(EEXIST)
            } else if nsems as usize > sets[idx].nsems {
                Err(EINVAL)
            } else {
                Ok(idx)
            }
        } else if semflg & IPC_CREAT == 0 && key != IPC_PRIVATE {
            Err(ENOENT)
        } else if nsems == 0 {
            Err(EINVAL)
        } else {
            match sets.iter().position(|s| !s.used) {
                None => Err(ENOSPC),
                Some(idx) => {
                    sets[idx] = EMPTY_SET;
                    sets[idx].used = true;
                    sets[idx].key = key;
                    sets[idx].nsems = nsems as usize;
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

/// struct sembuf { sem_num u16, sem_op i16, sem_flg i16 }（6 字节对齐到 8）
///
/// 尝试原子执行一组操作；不可满足返回 false（由调用方决定睡还是 EAGAIN）。
/// 全组要么全做要么不做（原版语义）。
fn try_semop(set: &mut SemSet, ops: &[(u16, i16, i16)]) -> bool {
    // 先全部验证可行性
    for &(num, op, _flg) in ops {
        let n = num as usize;
        if n >= set.nsems {
            return false;
        }
        let v = set.vals[n];
        if op < 0 && v + (op as i32) < 0 {
            return false;
        }
        if op > 0 && v + (op as i32) > SEMVMX {
            return false;
        }
        if op == 0 && v != 0 {
            return false;
        }
    }
    // 验证通过才落账
    for &(num, op, _flg) in ops {
        let n = num as usize;
        // 同一信号量可能被同组多个 op 触及，逐个落（分组语义允许）
        if op > 0 {
            set.vals[n] = (set.vals[n] + op as i32).min(SEMVMX);
        } else if op < 0 {
            set.vals[n] = (set.vals[n] + op as i32).max(0);
        }
        // op==0 是「等到零」判定，不改变计数
    }
    true
}

/// semop/semtimedop 公共体。`deadline` 为 None 表示无限等。
pub fn sem_op_timed(id: usize, sops: *const u8, nsops: usize, deadline: Option<u64>) -> i64 {
    if sops.is_null() || nsops == 0 || nsops > 32 {
        return -(EINVAL as i64);
    }
    let mut ops = [(0u16, 0i16, 0i16); 32];
    for i in 0..nsops {
        // SAFETY: 用户指针；sembuf 三个 2 字节字段连续（struct 有 2 字节
        // 对齐 padding 的话是 6 字节，glibc 定义为 6 字节 packed 到 8）。
        unsafe {
            let p = sops.add(i * 8);
            ops[i] = (
                (p as *const u16).read_unaligned(),
                (p.add(2) as *const i16).read_unaligned(),
                (p.add(4) as *const i16).read_unaligned(),
            );
        }
    }
    let ops = &ops[..nsops];
    loop {
        // SAFETY: 临界区试做整组操作。
        let r = unsafe {
            let flags = crate::irq::local_irq_save();
            let sets = &mut *core::ptr::addr_of_mut!(SETS);
            let r = if id >= MAX_SETS || !sets[id].used {
                Err(EIDRM)
            } else if ops.iter().any(|&(n, _, _)| n as usize >= sets[id].nsems) {
                Err(EINVAL)
            } else {
                Ok(try_semop(&mut sets[id], ops))
            };
            crate::irq::restore_flags(flags);
            r
        };
        match r {
            Err(e) => return -(e as i64),
            Ok(true) => return 0,
            Ok(false) => {
                // 任一 op 带 IPC_NOWAIT → 立即失败
                if ops.iter().any(|&(_, _, f)| f as i32 & IPC_NOWAIT != 0) {
                    return -(EAGAIN as i64);
                }
                if let Some(d) = deadline {
                    if crate::sched::jiffies() >= d {
                        return -(EAGAIN as i64);
                    }
                }
                // 睡到计数变化。合作式调度：其他任务/中断会得到 CPU。
                // SAFETY: 系统调用上下文让出。
                unsafe { crate::sched::schedule() };
            }
        }
    }
}

/// semctl(id, semnum, cmd, arg)。arg 的语义按 cmd：SETVAL 是 int 值，
/// GETALL/SETALL 是用户 u16 数组指针。
pub fn sys_semctl(id: usize, semnum: usize, cmd: i32, arg: u64) -> i64 {
    match cmd {
        IPC_RMID => {
            // SAFETY: 临界区。
            unsafe {
                let flags = crate::irq::local_irq_save();
                let sets = &mut *core::ptr::addr_of_mut!(SETS);
                if id >= MAX_SETS || !sets[id].used {
                    crate::irq::restore_flags(flags);
                    return -(EINVAL as i64);
                }
                sets[id] = EMPTY_SET;
                crate::irq::restore_flags(flags);
            }
            0
        }
        GETVAL => {
            // SAFETY: 临界区。
            unsafe {
                let flags = crate::irq::local_irq_save();
                let sets = &*core::ptr::addr_of!(SETS);
                let r = if id >= MAX_SETS || !sets[id].used || semnum >= sets[id].nsems {
                    Err(EINVAL)
                } else {
                    Ok(sets[id].vals[semnum])
                };
                crate::irq::restore_flags(flags);
                match r {
                    Ok(v) => v as i64,
                    Err(e) => -(e as i64),
                }
            }
        }
        SETVAL => {
            let v = arg as i32;
            if v < 0 || v > SEMVMX {
                return -(ERANGE as i64);
            }
            // SAFETY: 临界区。
            unsafe {
                let flags = crate::irq::local_irq_save();
                let sets = &mut *core::ptr::addr_of_mut!(SETS);
                if id >= MAX_SETS || !sets[id].used || semnum >= sets[id].nsems {
                    crate::irq::restore_flags(flags);
                    return -(EINVAL as i64);
                }
                sets[id].vals[semnum] = v;
                crate::irq::restore_flags(flags);
            }
            0
        }
        GETALL => {
            let out = arg as *mut u16;
            if out.is_null() {
                return -(EINVAL as i64);
            }
            // SAFETY: 临界区读，用户指针恒等映射可写。
            unsafe {
                let flags = crate::irq::local_irq_save();
                let sets = &*core::ptr::addr_of!(SETS);
                if id >= MAX_SETS || !sets[id].used {
                    crate::irq::restore_flags(flags);
                    return -(EINVAL as i64);
                }
                for i in 0..sets[id].nsems {
                    out.add(i).write_volatile(sets[id].vals[i] as u16);
                }
                crate::irq::restore_flags(flags);
            }
            0
        }
        SETALL => {
            let inp = arg as *const u16;
            if inp.is_null() {
                return -(EINVAL as i64);
            }
            // SAFETY: 临界区读用户数组并落账。
            unsafe {
                let flags = crate::irq::local_irq_save();
                let sets = &mut *core::ptr::addr_of_mut!(SETS);
                if id >= MAX_SETS || !sets[id].used {
                    crate::irq::restore_flags(flags);
                    return -(EINVAL as i64);
                }
                for i in 0..sets[id].nsems {
                    let v = inp.add(i).read_volatile() as i32;
                    if v > SEMVMX {
                        crate::irq::restore_flags(flags);
                        return -(ERANGE as i64);
                    }
                    sets[id].vals[i] = v;
                }
                crate::irq::restore_flags(flags);
            }
            0
        }
        _ => -(EINVAL as i64),
    }
}
