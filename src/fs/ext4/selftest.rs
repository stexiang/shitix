//! ext4 解析器自检
//!
//! 所有测试在纯内存中运行，不读磁盘，可在 start_kernel 早期调用。
//! 使用 serial::print / serial::print_dec 直接输出，完全避免 format_args!，
//! 以便在受限的内核内存布局下保持代码体积最小。

use super::{
    Ext4SuperBlock, Ext4GroupDesc, Ext4Inode, ExtentHeader, ExtentNode,
    Extent, Ext4DirEntry,
};
use super::dir::{DirIter, find_entry};
use super::group_desc::{ino_to_group, ino_to_disk};

/// 输出字符串到串口（不使用 format_args!）。
#[inline(never)]
fn puts(s: &str) {
    crate::serial::print(s);
}

/// 输出 u64 十进制数到串口（不使用 format_args!）。
#[inline(never)]
fn putn(v: u64) {
    crate::serial::print_dec(v);
}

/// 运行所有 ext4 自检。失败时打印标签，最后汇报通过率。
#[cold]
#[inline(never)]
pub fn ext4_selftest() {
    puts("--- ext4 selftest ---\n");
    let mut pass = 0u32;
    let mut fail = 0u32;

    // 静默通过：pass 只计数。失败时打印标签。
    macro_rules! check {
        ($tag:literal, $ok:expr) => {{
            if $ok {
                pass += 1;
            } else {
                puts("ext4 FAIL: ");
                puts($tag);
                puts("\n");
                fail += 1;
            }
        }};
    }

    // ---- 1. 超级块 ----
    {
        // 只需要前 104 字节（s_feature_ro_compat 结束于 offset 103）
        let mut raw = [0u8; 128];
        raw[56] = 0x53; raw[57] = 0xEF;  // magic
        raw[24] = 2;                       // log_block_size → 4096
        raw[32] = 0x00; raw[33] = 0x20;   // blocks_per_group = 8192
        raw[4]  = 0x00; raw[5]  = 0x20;   // blocks_count_lo = 8192
        raw[76] = 1;                       // rev_level = 1
        raw[88] = 0;    raw[89] = 1;       // inode_size = 256
        raw[96] = 0x42;                    // incompat: EXTENTS|FILETYPE
        raw[100]= 0x02;                    // ro_compat: LARGE_FILE

        // SAFETY: 128 字节，字段全覆盖测试所需偏移。
        let sb = unsafe { Ext4SuperBlock::from_slice(&raw) };
        check!("sb magic",        sb.is_valid());
        check!("sb block_size",   sb.block_size() == 4096);
        check!("sb group_count",  sb.group_count() == 1);
        check!("sb inode_size",   sb.inode_size() == 256);
        check!("sb has_extent",   sb.has_extent());
        check!("sb large_file",   sb.has_large_file());
        check!("sb not 64bit",    !sb.is_64bit());

        let mut raw2 = raw;
        raw2[4] = 0x01; raw2[5] = 0x20;   // blocks_count_lo = 8193
        let sb2 = unsafe { Ext4SuperBlock::from_slice(&raw2) };
        check!("sb group_count2", sb2.group_count() == 2);
    }

    // ---- 2. 块组描述符 32 字节 ----
    {
        let mut d = [0u8; 32];
        d[0] = 5; d[4] = 6; d[8] = 7;
        d[12] = 200; d[14] = 64;
        let g = Ext4GroupDesc::from_bytes(&d, 32).unwrap();
        check!("gd bitmap",       g.block_bitmap == 5);
        check!("gd inode_tbl",    g.inode_table  == 7);
        check!("gd free_blk",     g.free_blocks_count == 200);
    }

    // ---- 3. 块组描述符 64 字节（高位字段）----
    {
        let mut d = [0u8; 64];
        // block_bitmap_lo = 1（d[0..4] LE u32 = 1）
        // block_bitmap_hi = 1（d[32..36] LE u32 = 1）→ 合并后 0x1_0000_0001
        d[0] = 1; d[32] = 1;
        let g = Ext4GroupDesc::from_bytes(&d, 64).unwrap();
        check!("gd 64bit hi",     g.block_bitmap == 0x1_0000_0001u64);
    }

    // ---- 4. inode ----
    {
        let mut raw = [0u8; 256];
        raw[0] = 0xED; raw[1] = 0x81;     // mode = 0x81ED (reg 0755)
        raw[2] = 0xE8; raw[3] = 0x03;     // uid = 1000
        raw[4] = 0x00; raw[5] = 0x10;     // size_lo = 4096
        raw[24]= 0xE8; raw[25]= 0x03;     // gid = 1000
        raw[26]= 1;                        // links_count
        raw[116+4] = 0x01;                 // uid_high = 1 → uid = 0x1_03E8
        raw[116+6] = 0x02;                 // gid_high = 2 → gid = 0x2_03E8
        raw[128]   = 28;                   // i_extra_isize

        let ino = Ext4Inode::from_bytes(&raw).unwrap();
        check!("ino is_reg",      ino.is_reg());
        check!("ino size",        ino.i_size() == 4096);
        check!("ino uid",         ino.uid() == 66536);
        check!("ino gid",         ino.gid() == 132072);

        // 目录：i_size_high 不参与 i_size
        let mut rd = [0u8; 128];
        rd[0] = 0xED; rd[1] = 0x41; rd[4] = 0x00; rd[5] = 0x10; rd[8] = 0xFF;
        let id = Ext4Inode::from_bytes(&rd).unwrap();
        check!("ino dir",         id.is_dir());
        check!("ino dir size",    id.i_size() == 4096);

        // 심볼릭 링크
        let mut rl = [0u8; 128];
        rl[0] = 0xFF; rl[1] = 0xA1; rl[4] = 7;
        let il = Ext4Inode::from_bytes(&rl).unwrap();
        check!("ino symlink",     il.is_symlink());
        check!("ino fast_sym",    il.is_fast_symlink());
    }

    // ---- 5. extent 头 ----
    {
        let mut h = [0u8; 12];
        h[0] = 0x0A; h[1] = 0xF3; h[2] = 1; h[4] = 4;
        let eh = unsafe { ExtentHeader::from_bytes(&h) };
        check!("eh valid",        eh.is_valid());
        check!("eh leaf",         eh.is_leaf());
        check!("eh entries",      eh.entries() == 1);

        let mut h2 = h; h2[0] = 0xFF;
        let eh2 = unsafe { ExtentHeader::from_bytes(&h2) };
        check!("eh bad magic",    !eh2.is_valid());
    }

    // ---- 6. extent 叶子查找 ----
    {
        let mut buf = [0u8; 24];
        buf[0]=0x0A; buf[1]=0xF3; buf[2]=1; buf[4]=4;
        buf[12]=10; buf[16]=10; buf[20]=100;   // block=10 len=10 phys=100
        let node = ExtentNode::parse(&buf).unwrap();
        let e = node.lookup_leaf(10).unwrap();
        check!("ext hit start",   e.ee_start() == 100);
        let e2 = node.lookup_leaf(15).unwrap();
        check!("ext hit mid",     e2.ee_start() + (15 - e2.ee_block()) as u64 == 105);
        check!("ext hole before", node.lookup_leaf(9).is_none());
        check!("ext hole after",  node.lookup_leaf(20).is_none());
    }

    // ---- 7. unwritten extent ----
    {
        let mut buf = [0u8; 24];
        buf[0]=0x0A; buf[1]=0xF3; buf[2]=1; buf[4]=4;
        buf[16]=0x05; buf[17]=0x80;   // ee_len = 0x8005 (unwritten, actual=5)
        buf[20]=200;
        let node = ExtentNode::parse(&buf).unwrap();
        let e = node.extent(0).unwrap();
        check!("ext unwritten",   e.is_unwritten());
        check!("ext unwr len",    e.ee_len() == 5);
        check!("ext unwr lookup", node.lookup_leaf(3).is_some());
        check!("ext unwr hole",   node.lookup_leaf(5).is_none());
    }

    // ---- 8. 48 位物理块号 ----
    {
        let mut buf = [0u8; 24];
        buf[0]=0x0A; buf[1]=0xF3; buf[2]=1; buf[4]=4;
        buf[16]=1; buf[18]=0x01;   // len=1, start_hi=1 → phys=0x1_0000_0000
        let node = ExtentNode::parse(&buf).unwrap();
        let e = node.extent(0).unwrap();
        check!("ext 48bit",       e.ee_start() == 0x0001_0000_0000u64);
    }

    // ---- 9. 两段不连续 ----
    {
        let mut buf = [0u8; 36];
        buf[0]=0x0A; buf[1]=0xF3; buf[2]=2; buf[4]=4;
        buf[12]=0;  buf[16]=10; buf[20..24].copy_from_slice(&1000u32.to_le_bytes());
        buf[24]=20; buf[28]=10; buf[32..36].copy_from_slice(&2000u32.to_le_bytes());
        let node = ExtentNode::parse(&buf).unwrap();
        check!("ext2 A",          node.lookup_leaf(5).map(|e| e.ee_start()) == Some(1000));
        check!("ext2 B",          node.lookup_leaf(25).map(|e| e.ee_start()) == Some(2000));
        check!("ext2 hole",       node.lookup_leaf(15).is_none());
    }

    // ---- 10. 目录项迭代 ----
    {
        let mut blk = [0u8; 52];
        // "." inode=2
        blk[0]=2; blk[4]=12; blk[6]=1; blk[7]=2; blk[8]=b'.';
        // ".." inode=3
        blk[12]=3; blk[16]=12; blk[18]=2; blk[19]=2; blk[20]=b'.'; blk[21]=b'.';
        // empty slot (inode=0)
        blk[28]=12;
        // "hi" inode=5
        blk[36]=5; blk[40]=16; blk[42]=2; blk[43]=1; blk[44]=b'h'; blk[45]=b'i';

        let mut count = 0u32;
        let mut last_ino = 0u32;
        for e in DirIter::new(&blk) {
            count += 1;
            last_ino = e.inode;
        }
        check!("dir count",       count == 4);
        check!("dir last",        last_ino == 5);
        check!("dir find dot",    find_entry(&blk, b".")   == Some(2));
        check!("dir find hi",     find_entry(&blk, b"hi")  == Some(5));
        check!("dir no nope",     find_entry(&blk, b"nope").is_none());
    }

    // ---- 11. 目录项损坏检测 ----
    {
        let mut b0 = [0u8; 16]; b0[0]=1;
        check!("dir bad len=0",   DirIter::new(&b0).next().is_none());

        let mut b1 = [0u8; 16]; b1[0]=1; b1[4]=3;
        check!("dir bad len%4",   DirIter::new(&b1).next().is_none());

        let mut b2 = [0u8; 16]; b2[0]=1; b2[4]=100;
        check!("dir bad overflow",DirIter::new(&b2).next().is_none());
    }

    // ---- 12. ino_to_group / ino_to_disk ----
    {
        check!("ino grp ino=1",   ino_to_group(1,   128) == Some((0, 0)));
        check!("ino grp ino=128", ino_to_group(128, 128) == Some((0, 127)));
        check!("ino grp ino=129", ino_to_group(129, 128) == Some((1, 0)));
        check!("ino grp ino=0",   ino_to_group(0,   128).is_none());
        check!("ino disk ino=1",  ino_to_disk(1, 128, 128, 1024, 7) == Some((7, 0)));
        check!("ino disk ino=9",  ino_to_disk(9, 128, 128, 1024, 7) == Some((8, 0)));
        check!("ino disk ino=2",  ino_to_disk(2, 128, 128, 1024, 7) == Some((7, 128)));
    }

    puts("ext4: selftest ");
    putn(pass as u64);
    puts("/");
    putn((pass + fail) as u64);
    if fail > 0 {
        puts(" FAILED (");
        putn(fail as u64);
        puts(" failures)");
    } else {
        puts(" all ok");
    }
    puts("\n");
}
