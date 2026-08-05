//! ext2 inode 操作实现
//! 
//! 参考 linux-1.0.9/fs/ext2/inode.c
//! 
//! 实现 ext2 文件系统的 inode 操作，包括读取、写入、创建和删除。

use crate::fs::buffer;
use crate::fs::inode::{self, FsType};
use crate::fs::super_block;
use crate::klib::errno::ENOENT;
use crate::klib::printk::Level;

/// 从磁盘读取 ext2 inode
///
/// # Arguments
/// * `n` - inode 槽位下标
/// 
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn read_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        let ip = inode::inode_ptr(n);
        
        // 检查是否是 ext2 文件系统
        if (*ip).i_op != FsType::Ext2 {
            return;
        }
        
        let sb_nr = (*ip).i_sb;
        if sb_nr == inode::NIL {
            return;
        }
        
        let dev = super_block::sb(sb_nr).s_dev;
        let ino = (*ip).i_ino;
        
        // TODO: 实现 ext2 inode 读取
        // 需要：
        // 1. 根据 inode 号计算块组
        // 2. 读取组描述符获取 inode 表位置
        // 3. 读取 inode 数据
        // 4. 解析 inode 填充 Inode 结构
        
        crate::pr_warn!("ext2: read_inode: inode {} not implemented", ino);
    }
}

/// 写入 ext2 inode 到磁盘
///
/// # Arguments
/// * `n` - inode 槽位下标
/// 
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn write_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        let ip = inode::inode_ptr(n);
        
        if (*ip).i_op != FsType::Ext2 {
            return;
        }
        
        if !(*ip).i_dirt {
            return;
        }
        
        let ino = (*ip).i_ino;
        crate::pr_warn!("ext2: write_inode: inode {} not implemented", ino);
        
        (*ip).i_dirt = false;
    }
}

/// 释放 ext2 inode
///
/// 当 inode 链接数为 0 时调用，释放 inode 占用的资源
///
/// # Arguments
/// * `n` - inode 槽位下标
pub unsafe fn put_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        let ip = inode::inode_ptr(n);
        
        if (*ip).i_op != FsType::Ext2 {
            return;
        }
        
        let ino = (*ip).i_ino;
        
        // 如果链接数为 0，释放数据块和 inode 位图
        if (*ip).i_nlink == 0 {
            // TODO: 实现 ext2 inode 释放
            // 1. 截断文件释放所有块
            // 2. 释放 inode 位图
            crate::pr_warn!("ext2: put_inode: releasing inode {} not implemented", ino);
        }
    }
}
