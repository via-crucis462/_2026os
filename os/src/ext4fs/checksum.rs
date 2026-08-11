//! Raw-byte helpers for ext4 metadata checksums.
//!
//! The helpers in this module intentionally do not depend on the in-memory
//! ext4 structs.  ext4 on-disk inodes and group descriptors may be larger than
//! the structs used by the filesystem implementation, and checksums must be
//! calculated over the complete on-disk representation.

/// The CRC32C polynomial in reflected form, as used by ext4 metadata_csum.
pub const CRC32C_POLY: u32 = 0x82F6_3B78;

/// The size of the historical ext4 inode body.
pub const EXT4_GOOD_OLD_INODE_SIZE: usize = 128;
/// Offset of `i_checksum_lo` in an ext4 inode.
pub const EXT4_INODE_CHECKSUM_LO_OFFSET: usize = 0x7c;
/// Offset of `i_checksum_hi` in an extended ext4 inode.
pub const EXT4_INODE_CHECKSUM_HI_OFFSET: usize = 0x82;
/// `i_checksum_hi` is valid only when `i_extra_isize` covers the field.
pub const EXT4_INODE_CSUM_HI_EXTRA_END: u16 = 4;

/// Offset of `bg_block_bitmap_csum_lo` in a group descriptor.
pub const EXT4_BG_BLOCK_BITMAP_CSUM_LO_OFFSET: usize = 0x18;
/// Offset of `bg_inode_bitmap_csum_lo` in a group descriptor.
pub const EXT4_BG_INODE_BITMAP_CSUM_LO_OFFSET: usize = 0x1a;
/// Offset of `bg_checksum` in a group descriptor.
pub const EXT4_BG_CHECKSUM_OFFSET: usize = 0x1e;
/// Offset of `bg_block_bitmap_csum_hi` in a 64-byte descriptor.
pub const EXT4_BG_BLOCK_BITMAP_CSUM_HI_OFFSET: usize = 0x38;
/// Offset of `bg_inode_bitmap_csum_hi` in a 64-byte descriptor.
pub const EXT4_BG_INODE_BITMAP_CSUM_HI_OFFSET: usize = 0x3a;

/// The ext4 superblock is 1024 bytes and its checksum occupies the last four.
pub const EXT4_SUPERBLOCK_SIZE: usize = 1024;
pub const EXT4_SUPERBLOCK_CHECKSUM_OFFSET: usize = 0x3fc;

/// Update a CRC32C value over `data`.
///
/// This is the same reflected, non-final-xor convention as the kernel's
/// `crc32c()`/`ext4_chksum()` helpers.  Passing the result back as `seed`
/// permits incremental checksums over discontiguous byte ranges.
#[inline]
pub fn crc32c(mut seed: u32, data: &[u8]) -> u32 {
    for &byte in data {
        seed ^= byte as u32;
        for _ in 0..8 {
            let mask = (seed & 1).wrapping_neg();
            seed = (seed >> 1) ^ (CRC32C_POLY & mask);
        }
    }
    seed
}

/// Derive the per-inode checksum seed used by directory and extent metadata.
#[inline]
pub fn inode_checksum_seed(fs_seed: u32, inode_number: u32, generation: u32) -> u32 {
    let inode_number = inode_number.to_le_bytes();
    let generation = generation.to_le_bytes();
    let seed = crc32c(fs_seed, &inode_number);
    crc32c(seed, &generation)
}

/// Calculate an inode checksum over a raw on-disk inode.
///
/// `inode` may be 128 bytes or any larger inode size supported by ext4.  The
/// low checksum field at `0x7c` is always covered as two zero bytes.  For an
/// inode large enough to contain `i_checksum_hi` at `0x82`, that field is also
/// covered as two zero bytes.  Existing checksum values therefore do not
/// influence the result.
pub fn inode_checksum(fs_seed: u32, inode_number: u32, inode: &[u8]) -> Option<u32> {
    if inode.len() < EXT4_GOOD_OLD_INODE_SIZE || inode.len() < EXT4_INODE_CHECKSUM_LO_OFFSET + 2 {
        return None;
    }

    // Keep the inode number in the API's input contract explicit.  ext4 folds
    // the inode number and generation into a per-inode seed before walking the
    // raw inode bytes.
    let inode_number = inode_number.to_le_bytes();
    let generation = if inode.len() >= 0x68 {
        u32::from_le_bytes([inode[0x64], inode[0x65], inode[0x66], inode[0x67]])
    } else {
        0
    };
    let mut inode_seed = crc32c(fs_seed, &inode_number);
    inode_seed = crc32c(inode_seed, &generation.to_le_bytes());

    // Recompute the byte stream using the per-inode seed.  Keeping this
    // explicit avoids relying on a temporary allocation and mirrors ext4's
    // precomputed i_csum_seed exactly.
    let mut checksum = crc32c(inode_seed, &inode[..EXT4_INODE_CHECKSUM_LO_OFFSET]);
    checksum = crc32c(checksum, &[0u8; 2]);
    checksum = crc32c(
        checksum,
        &inode[EXT4_INODE_CHECKSUM_LO_OFFSET + 2..EXT4_GOOD_OLD_INODE_SIZE],
    );
    if inode.len() > EXT4_GOOD_OLD_INODE_SIZE {
        let before_hi_end = core::cmp::min(EXT4_INODE_CHECKSUM_HI_OFFSET, inode.len());
        if before_hi_end > EXT4_GOOD_OLD_INODE_SIZE {
            checksum = crc32c(checksum, &inode[EXT4_GOOD_OLD_INODE_SIZE..before_hi_end]);
        }
        let has_checksum_hi = inode.len() >= EXT4_INODE_CHECKSUM_HI_OFFSET + 2
            && u16::from_le_bytes([
                inode[EXT4_GOOD_OLD_INODE_SIZE],
                inode[EXT4_GOOD_OLD_INODE_SIZE + 1],
            ]) >= EXT4_INODE_CSUM_HI_EXTRA_END;
        if has_checksum_hi {
            checksum = crc32c(checksum, &[0u8; 2]);
        } else if inode.len() >= EXT4_INODE_CHECKSUM_HI_OFFSET + 2 {
            checksum = crc32c(
                checksum,
                &inode[EXT4_INODE_CHECKSUM_HI_OFFSET..EXT4_INODE_CHECKSUM_HI_OFFSET + 2],
            );
        }
        let after_hi = EXT4_INODE_CHECKSUM_HI_OFFSET + 2;
        if inode.len() > after_hi {
            checksum = crc32c(checksum, &inode[after_hi..]);
        }
    }

    Some(checksum)
}

/// Set both checksum halves in a raw inode, when those fields fit.
pub fn set_inode_checksum(fs_seed: u32, inode_number: u32, inode: &mut [u8]) -> Option<u32> {
    let checksum = inode_checksum(fs_seed, inode_number, inode)?;
    inode[EXT4_INODE_CHECKSUM_LO_OFFSET..EXT4_INODE_CHECKSUM_LO_OFFSET + 2]
        .copy_from_slice(&(checksum as u16).to_le_bytes());
    let has_checksum_hi = inode.len() >= EXT4_INODE_CHECKSUM_HI_OFFSET + 2
        && u16::from_le_bytes([
            inode[EXT4_GOOD_OLD_INODE_SIZE],
            inode[EXT4_GOOD_OLD_INODE_SIZE + 1],
        ]) >= EXT4_INODE_CSUM_HI_EXTRA_END;
    if has_checksum_hi {
        inode[EXT4_INODE_CHECKSUM_HI_OFFSET..EXT4_INODE_CHECKSUM_HI_OFFSET + 2]
            .copy_from_slice(&((checksum >> 16) as u16).to_le_bytes());
    }
    Some(checksum)
}

/// Calculate a bitmap checksum over exactly the valid bitmap bytes supplied.
#[inline]
pub fn bitmap_checksum(seed: u32, bitmap: &[u8]) -> u32 {
    crc32c(seed, bitmap)
}

/// Kind of bitmap checksum field in a group descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BitmapKind {
    Block,
    Inode,
}

/// Store a block or inode bitmap checksum in a group descriptor.
///
/// The low half is present in both 32-byte and 64-byte descriptors.  The high
/// half is written when the supplied descriptor is large enough to contain the
/// 64-bit-descriptor field.
pub fn set_bitmap_checksum(
    seed: u32,
    bitmap: &[u8],
    descriptor: &mut [u8],
    kind: BitmapKind,
) -> Option<u32> {
    let (low_offset, high_offset) = match kind {
        BitmapKind::Block => (
            EXT4_BG_BLOCK_BITMAP_CSUM_LO_OFFSET,
            EXT4_BG_BLOCK_BITMAP_CSUM_HI_OFFSET,
        ),
        BitmapKind::Inode => (
            EXT4_BG_INODE_BITMAP_CSUM_LO_OFFSET,
            EXT4_BG_INODE_BITMAP_CSUM_HI_OFFSET,
        ),
    };
    if descriptor.len() < low_offset + 2 {
        return None;
    }

    let checksum = bitmap_checksum(seed, bitmap);
    descriptor[low_offset..low_offset + 2].copy_from_slice(&(checksum as u16).to_le_bytes());
    if descriptor.len() >= high_offset + 2 {
        descriptor[high_offset..high_offset + 2]
            .copy_from_slice(&((checksum >> 16) as u16).to_le_bytes());
    }
    Some(checksum)
}

#[inline]
pub fn set_block_bitmap_checksum(seed: u32, bitmap: &[u8], descriptor: &mut [u8]) -> Option<u32> {
    set_bitmap_checksum(seed, bitmap, descriptor, BitmapKind::Block)
}

#[inline]
pub fn set_inode_bitmap_checksum(seed: u32, bitmap: &[u8], descriptor: &mut [u8]) -> Option<u32> {
    set_bitmap_checksum(seed, bitmap, descriptor, BitmapKind::Inode)
}

/// Calculate the low-16-bit group descriptor checksum used by metadata_csum.
pub fn group_desc_checksum(seed: u32, group: u32, descriptor: &[u8]) -> Option<u16> {
    if descriptor.len() < EXT4_BG_CHECKSUM_OFFSET + 2 {
        return None;
    }

    let mut checksum = crc32c(seed, &group.to_le_bytes());
    checksum = crc32c(checksum, &descriptor[..EXT4_BG_CHECKSUM_OFFSET]);
    checksum = crc32c(checksum, &[0u8; 2]);
    if descriptor.len() > EXT4_BG_CHECKSUM_OFFSET + 2 {
        checksum = crc32c(checksum, &descriptor[EXT4_BG_CHECKSUM_OFFSET + 2..]);
    }
    Some(checksum as u16)
}

/// Set `bg_checksum` in a 32-byte or 64-byte raw group descriptor.
pub fn set_group_desc_checksum(seed: u32, group: u32, descriptor: &mut [u8]) -> Option<u16> {
    let checksum = group_desc_checksum(seed, group, descriptor)?;
    descriptor[EXT4_BG_CHECKSUM_OFFSET..EXT4_BG_CHECKSUM_OFFSET + 2]
        .copy_from_slice(&checksum.to_le_bytes());
    Some(checksum)
}

/// Calculate a raw ext4 superblock checksum.
pub fn superblock_checksum(superblock: &[u8]) -> Option<u32> {
    if superblock.len() < EXT4_SUPERBLOCK_SIZE {
        return None;
    }
    Some(crc32c(
        !0u32,
        &superblock[..EXT4_SUPERBLOCK_CHECKSUM_OFFSET],
    ))
}

/// Set `s_checksum` in a raw ext4 superblock.
pub fn set_superblock_checksum(superblock: &mut [u8]) -> Option<u32> {
    let checksum = superblock_checksum(superblock)?;
    superblock[EXT4_SUPERBLOCK_CHECKSUM_OFFSET..EXT4_SUPERBLOCK_SIZE]
        .copy_from_slice(&checksum.to_le_bytes());
    Some(checksum)
}

/// Calculate a directory leaf checksum using a precomputed inode seed.
pub fn dir_leaf_checksum_with_inode_seed(inode_seed: u32, block: &[u8]) -> Option<u32> {
    if block.len() < 12 {
        return None;
    }
    let tail_offset = block.len() - 12;
    if block[tail_offset..tail_offset + 4] != [0, 0, 0, 0]
        || u16::from_le_bytes([block[tail_offset + 4], block[tail_offset + 5]]) != 12
        || block[tail_offset + 6] != 0
        || block[tail_offset + 7] != 0xde
    {
        return None;
    }
    Some(crc32c(inode_seed, &block[..tail_offset]))
}

/// Set the checksum in the fake 12-byte tail of a directory leaf block.
pub fn set_dir_leaf_checksum_with_inode_seed(inode_seed: u32, block: &mut [u8]) -> Option<u32> {
    let checksum = dir_leaf_checksum_with_inode_seed(inode_seed, block)?;
    let tail_offset = block.len() - 12;
    block[tail_offset + 8..tail_offset + 12].copy_from_slice(&checksum.to_le_bytes());
    Some(checksum)
}

/// Calculate a directory leaf checksum from filesystem seed, inode number,
/// and inode generation.
#[inline]
pub fn dir_leaf_checksum(
    fs_seed: u32,
    inode_number: u32,
    generation: u32,
    block: &[u8],
) -> Option<u32> {
    dir_leaf_checksum_with_inode_seed(
        inode_checksum_seed(fs_seed, inode_number, generation),
        block,
    )
}

/// Set a directory leaf checksum from filesystem seed, inode number, and
/// inode generation.
#[inline]
pub fn set_dir_leaf_checksum(
    fs_seed: u32,
    inode_number: u32,
    generation: u32,
    block: &mut [u8],
) -> Option<u32> {
    set_dir_leaf_checksum_with_inode_seed(
        inode_checksum_seed(fs_seed, inode_number, generation),
        block,
    )
}

fn extent_tail_offset(block: &[u8]) -> Option<usize> {
    if block.len() < 12 {
        return None;
    }
    let max_entries = u16::from_le_bytes([block[4], block[5]]) as usize;
    let entries_bytes = max_entries.checked_mul(12)?;
    let tail_offset = 12usize.checked_add(entries_bytes)?;
    if tail_offset.checked_add(4)? > block.len() {
        return None;
    }
    Some(tail_offset)
}

/// Calculate an external extent-node checksum using a precomputed inode seed.
pub fn extent_node_checksum_with_inode_seed(inode_seed: u32, block: &[u8]) -> Option<u32> {
    let tail_offset = extent_tail_offset(block)?;
    Some(crc32c(inode_seed, &block[..tail_offset]))
}

/// Set the checksum in an external extent node's four-byte tail.
pub fn set_extent_node_checksum_with_inode_seed(inode_seed: u32, block: &mut [u8]) -> Option<u32> {
    let checksum = extent_node_checksum_with_inode_seed(inode_seed, block)?;
    let tail_offset = extent_tail_offset(block)?;
    block[tail_offset..tail_offset + 4].copy_from_slice(&checksum.to_le_bytes());
    Some(checksum)
}

/// Calculate an external extent-node checksum from filesystem seed, inode
/// number, and inode generation.
#[inline]
pub fn extent_node_checksum(
    fs_seed: u32,
    inode_number: u32,
    generation: u32,
    block: &[u8],
) -> Option<u32> {
    extent_node_checksum_with_inode_seed(
        inode_checksum_seed(fs_seed, inode_number, generation),
        block,
    )
}

/// Set an external extent-node checksum from filesystem seed, inode number,
/// and inode generation.
#[inline]
pub fn set_extent_node_checksum(
    fs_seed: u32,
    inode_number: u32,
    generation: u32,
    block: &mut [u8],
) -> Option<u32> {
    set_extent_node_checksum_with_inode_seed(
        inode_checksum_seed(fs_seed, inode_number, generation),
        block,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_matches_kernel_raw_convention() {
        assert_eq!(crc32c(!0, b"123456789"), 0x1cf9_6d7c);
        let split = crc32c(crc32c(!0, b"1234"), b"56789");
        assert_eq!(split, crc32c(!0, b"123456789"));
    }

    #[test]
    fn inode_checksum_covers_extended_inode() {
        let mut inode = [0u8; 256];
        inode[0x64..0x68].copy_from_slice(&0x1122_3344u32.to_le_bytes());
        inode[0x80..0x82].copy_from_slice(&0x5566u16.to_le_bytes());
        inode[0xf0] = 0xa5;
        let checksum = set_inode_checksum(0xdead_beef, 42, &mut inode).unwrap();
        assert_eq!(inode_checksum(0xdead_beef, 42, &inode), Some(checksum));
        inode[0x7c] ^= 1;
        assert_eq!(inode_checksum(0xdead_beef, 42, &inode), Some(checksum));
        inode[0x82] ^= 1;
        assert_eq!(inode_checksum(0xdead_beef, 42, &inode), Some(checksum));
    }

    #[test]
    fn bitmap_and_group_descriptor_setters_write_expected_halves() {
        let bitmap = [0x5au8; 1024];
        let mut desc = [0u8; 64];
        let bitmap_sum = set_block_bitmap_checksum(7, &bitmap, &mut desc).unwrap();
        assert_eq!(
            u16::from_le_bytes([desc[0x18], desc[0x19]]),
            bitmap_sum as u16
        );
        assert_eq!(
            u16::from_le_bytes([desc[0x38], desc[0x39]]),
            (bitmap_sum >> 16) as u16
        );

        let desc_sum = set_group_desc_checksum(7, 3, &mut desc).unwrap();
        assert_eq!(u16::from_le_bytes([desc[0x1e], desc[0x1f]]), desc_sum);
        assert_eq!(group_desc_checksum(7, 3, &desc), Some(desc_sum));
    }

    #[test]
    fn superblock_directory_and_extent_setters_round_trip() {
        let mut superblock = [0u8; EXT4_SUPERBLOCK_SIZE];
        superblock[0x10] = 0x33;
        let super_sum = set_superblock_checksum(&mut superblock).unwrap();
        assert_eq!(superblock_checksum(&superblock), Some(super_sum));

        let mut dir = [0u8; 4096];
        let tail = 4096 - 12;
        dir[tail + 4..tail + 6].copy_from_slice(&12u16.to_le_bytes());
        dir[tail + 7] = 0xde;
        let dir_sum = set_dir_leaf_checksum(0x1234, 9, 2, &mut dir).unwrap();
        assert_eq!(dir_leaf_checksum(0x1234, 9, 2, &dir), Some(dir_sum));

        let mut extent = [0u8; 4096];
        extent[0..2].copy_from_slice(&0xf30au16.to_le_bytes());
        extent[4..6].copy_from_slice(&340u16.to_le_bytes());
        extent[123] = 0x9b;
        let extent_sum = set_extent_node_checksum(0x1234, 9, 2, &mut extent).unwrap();
        assert_eq!(
            extent_node_checksum(0x1234, 9, 2, &extent),
            Some(extent_sum)
        );
    }
}
