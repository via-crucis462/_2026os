pub const EXT4_DIR_ENTRY_HEADER_LEN: usize = 8;
pub const EXT4_DIR_ENTRY_MAX_NAME_LEN: usize = u8::MAX as usize;

fn decode_header(buf: &[u8]) -> Option<(u32, u16, u8, u8)> {
    let header = buf.get(..EXT4_DIR_ENTRY_HEADER_LEN)?;
    let inode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let rec_len = u16::from_le_bytes([header[4], header[5]]);
    let name_len = header[6];
    let file_type = header[7];
    let rec_len_usize = rec_len as usize;

    if rec_len_usize < EXT4_DIR_ENTRY_HEADER_LEN
        || rec_len_usize % 4 != 0
        || rec_len_usize > buf.len()
        || name_len as usize > rec_len_usize - EXT4_DIR_ENTRY_HEADER_LEN
    {
        return None;
    }

    Some((inode, rec_len, name_len, file_type))
}

/// A validated view of one variable-length ext4 directory entry.
///
/// Directory entries are stored as an 8-byte header followed by `name_len`
/// bytes. They must not be cast to a Rust struct with a fixed `[u8; 255]`
/// tail: a valid entry near the end of a block can be much shorter than that.
#[derive(Clone, Copy, Debug)]
pub struct Ext4DirEntry<'a> {
    bytes: &'a [u8],
    inode: u32,
    rec_len: u16,
    name_len: u8,
    file_type: u8,
}

impl<'a> Ext4DirEntry<'a> {
    /// Parse and validate one on-disk directory entry from the beginning of
    /// `buf`. The returned view is restricted to the entry's `rec_len`.
    pub fn from_bytes(buf: &'a [u8]) -> Option<Self> {
        let (inode, rec_len, name_len, file_type) = decode_header(buf)?;

        Some(Self {
            bytes: &buf[..rec_len as usize],
            inode,
            rec_len,
            name_len,
            file_type,
        })
    }

    pub fn inode(&self) -> u32 {
        self.inode
    }

    pub fn name_len(&self) -> u8 {
        self.name_len
    }

    pub fn rec_len(&self) -> u16 {
        self.rec_len
    }

    pub fn file_type(&self) -> u8 {
        self.file_type
    }

    /// Return the raw on-disk filename bytes. ext4 names are byte strings,
    /// not UTF-8 strings.
    pub fn name_bytes(&self) -> &'a [u8] {
        &self.bytes[EXT4_DIR_ENTRY_HEADER_LEN..EXT4_DIR_ENTRY_HEADER_LEN + self.name_len as usize]
    }

    /// Convert ext4's file type to the Linux `DT_*` value used by dirent64.
    pub fn linux_dirent_type(&self) -> u8 {
        match self.file_type {
            1 => 8,  // EXT4_FT_REG_FILE -> DT_REG
            2 => 4,  // EXT4_FT_DIR      -> DT_DIR
            3 => 2,  // EXT4_FT_CHRDEV   -> DT_CHR
            4 => 6,  // EXT4_FT_BLKDEV   -> DT_BLK
            5 => 1,  // EXT4_FT_FIFO     -> DT_FIFO
            6 => 12, // EXT4_FT_SOCK     -> DT_SOCK
            7 => 10, // EXT4_FT_SYMLINK  -> DT_LNK
            _ => 0,  // EXT4_FT_UNKNOWN  -> DT_UNKNOWN
        }
    }

    /// Return the aligned minimum record size for this entry's name.
    pub fn real_len(&self) -> u16 {
        Self::record_len(self.name_len as usize)
            .expect("an ext4 directory name length always fits its record") as u16
    }

    /// Return the aligned minimum record size for a name of `name_len` bytes.
    pub fn record_len(name_len: usize) -> Option<usize> {
        if name_len > EXT4_DIR_ENTRY_MAX_NAME_LEN {
            return None;
        }
        Some((EXT4_DIR_ENTRY_HEADER_LEN + name_len + 3) & !3)
    }

    /// Encode a complete directory record in `buf`.
    ///
    /// `rec_len` may include reusable slack after the name, as ext4 requires.
    /// The slack is cleared so no prior directory data is retained in it.
    pub fn write_to(buf: &mut [u8], inode: u32, rec_len: u16, name: &[u8], file_type: u8) -> bool {
        let Some(min_len) = Self::record_len(name.len()) else {
            return false;
        };
        let rec_len_usize = rec_len as usize;
        if rec_len_usize < min_len || rec_len_usize > buf.len() || rec_len_usize % 4 != 0 {
            return false;
        }

        let record = &mut buf[..rec_len_usize];
        record.fill(0);
        record[..4].copy_from_slice(&inode.to_le_bytes());
        record[4..6].copy_from_slice(&rec_len.to_le_bytes());
        record[6] = name.len() as u8;
        record[7] = file_type;
        record[EXT4_DIR_ENTRY_HEADER_LEN..EXT4_DIR_ENTRY_HEADER_LEN + name.len()]
            .copy_from_slice(name);
        true
    }

    /// Update an already validated entry's inode field.
    pub fn set_inode(buf: &mut [u8], inode: u32) -> bool {
        if decode_header(buf).is_none() {
            return false;
        }
        buf[..4].copy_from_slice(&inode.to_le_bytes());
        true
    }

    /// Update an already validated entry's file type field.
    pub fn set_file_type(buf: &mut [u8], file_type: u8) -> bool {
        if decode_header(buf).is_none() {
            return false;
        }
        buf[7] = file_type;
        true
    }

    /// Update an entry's record length while preserving its header and name.
    pub fn set_rec_len(buf: &mut [u8], rec_len: u16) -> bool {
        let Some((_, _, name_len, _)) = decode_header(buf) else {
            return false;
        };
        let min_len = Self::record_len(name_len as usize)
            .expect("an ext4 directory name length always fits its record");
        let rec_len_usize = rec_len as usize;
        if rec_len_usize < min_len || rec_len_usize > buf.len() || rec_len_usize % 4 != 0 {
            return false;
        }

        buf[4..6].copy_from_slice(&rec_len.to_le_bytes());
        true
    }

    /// ext4 metadata-checksum directories reserve this fake final entry.
    pub fn is_checksum_tail(&self) -> bool {
        self.inode == 0 && self.rec_len == 12 && self.name_len == 0 && self.file_type == 0xDE
    }
}

#[cfg(test)]
mod tests {
    use super::Ext4DirEntry;

    #[test]
    fn parses_a_short_entry_without_a_fixed_size_tail() {
        let bytes = [
            0x78, 0x56, 0x34, 0x12, // inode
            12, 0, // rec_len
            3, 1, // name_len, file_type
            b'f', b'o', b'o', 0,
        ];

        let entry = Ext4DirEntry::from_bytes(&bytes).unwrap();
        assert_eq!(entry.inode(), 0x1234_5678);
        assert_eq!(entry.rec_len(), 12);
        assert_eq!(entry.name_bytes(), b"foo");
        assert_eq!(entry.linux_dirent_type(), 8);
    }

    #[test]
    fn preserves_non_utf8_name_bytes() {
        let mut bytes = [0xa5; 12];
        assert!(Ext4DirEntry::write_to(
            &mut bytes,
            7,
            12,
            &[0xff, b'x', 0x80],
            7,
        ));

        let entry = Ext4DirEntry::from_bytes(&bytes).unwrap();
        assert_eq!(entry.name_bytes(), &[0xff, b'x', 0x80]);
        assert_eq!(entry.linux_dirent_type(), 10);
        assert_eq!(bytes[11], 0);
    }

    #[test]
    fn rejects_invalid_record_lengths_and_name_bounds() {
        let truncated = [1, 0, 0, 0, 16, 0, 1, 1, b'x', 0, 0, 0];
        assert!(Ext4DirEntry::from_bytes(&truncated).is_none());

        let name_overrun = [1, 0, 0, 0, 8, 0, 1, 1];
        assert!(Ext4DirEntry::from_bytes(&name_overrun).is_none());

        let unaligned = [1, 0, 0, 0, 10, 0, 1, 1, b'x', 0];
        assert!(Ext4DirEntry::from_bytes(&unaligned).is_none());
    }

    #[test]
    fn updates_only_valid_entries() {
        let mut bytes = [0u8; 16];
        assert!(Ext4DirEntry::write_to(&mut bytes, 1, 16, b"a", 1));
        assert!(Ext4DirEntry::set_inode(&mut bytes, 2));
        assert!(Ext4DirEntry::set_file_type(&mut bytes, 2));
        assert!(Ext4DirEntry::set_rec_len(&mut bytes, 12));

        let entry = Ext4DirEntry::from_bytes(&bytes).unwrap();
        assert_eq!(entry.inode(), 2);
        assert_eq!(entry.file_type(), 2);
        assert_eq!(entry.rec_len(), 12);
        assert!(!Ext4DirEntry::set_rec_len(&mut bytes, 8));
        assert!(!Ext4DirEntry::set_inode(&mut [0; 7], 1));
    }

    #[test]
    fn recognizes_checksum_tail_and_maps_file_types() {
        let mut tail = [0u8; 12];
        assert!(Ext4DirEntry::write_to(&mut tail, 0, 12, b"", 0xDE));
        assert!(Ext4DirEntry::from_bytes(&tail).unwrap().is_checksum_tail());

        let expected = [0, 8, 4, 2, 6, 1, 12, 10];
        for (ext4_type, linux_type) in expected.iter().copied().enumerate() {
            assert!(Ext4DirEntry::write_to(
                &mut tail,
                1,
                12,
                b"x",
                ext4_type as u8,
            ));
            assert_eq!(
                Ext4DirEntry::from_bytes(&tail).unwrap().linux_dirent_type(),
                linux_type,
            );
        }
    }
}
