use core::ptr;
use core::time::Duration;

use super::{Cluster, Fat32Operations, file::Fat32FileNode, reader::Fat32Reader};
use crate::{
    error::{FsError, KernelError, Result},
    fs::{
        DirStream, Dirent, FileType, Inode, InodeId,
        attr::{FileAttr, FilePermissions},
    },
};
use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use async_trait::async_trait;
use log::warn;

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    struct Fat32Attributes: u8 {
        const READ_ONLY    = 0x01;
        const HIDDEN       = 0x02;
        const SYSTEM       = 0x04;
        const VOLUME_LABEL = 0x08;
        const DIRECTORY    = 0x10;
        const ARCHIVE      = 0x20;
        const DEVICE       = 0x40;
    }
}

impl TryFrom<Fat32Attributes> for FileType {
    type Error = KernelError;

    fn try_from(value: Fat32Attributes) -> Result<Self> {
        if value.contains(Fat32Attributes::DIRECTORY) {
            Ok(FileType::Directory)
        } else if value.contains(Fat32Attributes::ARCHIVE) {
            Ok(FileType::File)
        } else {
            warn!("Entry is neither a regular file nor a directory. Ignoring.");
            Err(FsError::InvalidFs.into())
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
struct LfnEntry {
    sequence_number: u8,
    name1: [u16; 5],    // Chars 1-5, UTF-16 LE
    attributes: u8,     // Always 0x0F
    entry_type: u8,     // Always 0x00
    checksum: u8,       // Checksum of the 8.3 name
    name2: [u16; 6],    // Chars 6-11
    first_cluster: u16, // Always 0x0000
    name3: [u16; 2],    // Chars 12-13
}

impl LfnEntry {
    fn extract_chars(&self) -> Vec<u16> {
        let mut chars = Vec::with_capacity(13);

        unsafe {
            let name1 = ptr::read_unaligned(&raw const self.name1);
            let name2 = ptr::read_unaligned(&raw const self.name2);
            let name3 = ptr::read_unaligned(&raw const self.name3);

            chars.extend_from_slice(&name1);
            chars.extend_from_slice(&name2);
            chars.extend_from_slice(&name3);
        }

        chars
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
struct DirEntry {
    dos_file_name: [u8; 8],
    dos_extension: [u8; 3],
    attributes: Fat32Attributes,
    _reserved: u8,
    ctime_ms: u8,
    ctime: u16,
    cdate: u16,
    adate: u16,
    clust_high: u16,
    mtime: u16,
    mdate: u16,
    clust_low: u16,
    size: u32,
}

impl DirEntry {
    pub fn parse_filename(&self) -> String {
        let name_part = self
            .dos_file_name
            .iter()
            .position(|&c| c == b' ')
            .unwrap_or(self.dos_file_name.len());

        // Find the end of the extension part
        let ext_part = self
            .dos_extension
            .iter()
            .position(|&c| c == b' ')
            .unwrap_or(self.dos_extension.len());

        let mut result = String::from_utf8_lossy(&self.dos_file_name[..name_part]).into_owned();

        if ext_part > 0 {
            result.push('.');
            result.push_str(&String::from_utf8_lossy(&self.dos_extension[..ext_part]));
        }

        result.to_ascii_lowercase()
    }
}

struct Fat32DirEntry {
    attr: FileAttr,
    cluster: Cluster,
    name: String,
    offset: u64,
}

struct Fat32DirStream<T: Fat32Operations> {
    reader: Fat32Reader<T>,
    offset: u64,
    lfn_buffer: Vec<u16>,
    fs_id: u64,
}

impl<T: Fat32Operations> Clone for Fat32DirStream<T> {
    fn clone(&self) -> Self {
        Self {
            reader: self.reader.clone(),
            offset: self.offset,
            lfn_buffer: self.lfn_buffer.clone(),
            fs_id: self.fs_id,
        }
    }
}

impl<T: Fat32Operations> Fat32DirStream<T> {
    pub fn new(fs: Arc<T>, root: Cluster) -> Self {
        let max_sz = fs.iter_clusters(root).count() as u64 * fs.bytes_per_cluster() as u64;
        let fs_id = fs.id();

        // For directory nodes, the size is 0. In our case, fake the size to be
        // the number of clusters in the chain such that we never read past the
        // end.
        Self {
            reader: Fat32Reader::new(fs, root, max_sz),
            offset: 0,
            lfn_buffer: Vec::new(),
            fs_id,
        }
    }

    pub fn advance(&mut self, offset: u64) {
        self.offset = offset;
    }

    async fn next_fat32_entry(&mut self) -> Result<Option<Fat32DirEntry>> {
        loop {
            let mut entry_bytes = [0; 32];
            self.reader
                .read_at(self.offset * 32, &mut entry_bytes)
                .await?;

            match entry_bytes[0] {
                0x00 => return Ok(None), // End of directory, no more entries
                0xE5 => {
                    // Deleted entry
                    self.lfn_buffer.clear();
                    self.offset += 1;
                    continue;
                }
                _ => (), // A normal entry
            }

            // Check attribute byte (offset 11) to differentiate LFN vs 8.3
            let attribute_byte = entry_bytes[11];
            if attribute_byte == 0x0F {
                // It's an LFN entry
                let lfn_entry: LfnEntry =
                    unsafe { ptr::read_unaligned(entry_bytes.as_ptr() as *const _) };

                // LFN entries are stored backwards, so we prepend.
                let new_chars = lfn_entry.extract_chars();
                self.lfn_buffer.splice(0..0, new_chars);

                self.offset += 1;
                continue;
            }

            let dir_entry: DirEntry =
                unsafe { ptr::read_unaligned(entry_bytes.as_ptr() as *const _) };

            if dir_entry.attributes.contains(Fat32Attributes::VOLUME_LABEL) {
                self.lfn_buffer.clear();
                self.offset += 1;
                continue;
            }

            let name = if !self.lfn_buffer.is_empty() {
                let len = self
                    .lfn_buffer
                    .iter()
                    .position(|&c| c == 0x0000 || c == 0xFFFF)
                    .unwrap_or(self.lfn_buffer.len());
                String::from_utf16_lossy(&self.lfn_buffer[..len])
            } else {
                // No LFN, parse the 8.3 name.
                dir_entry.parse_filename()
            };

            // Process the metadata from the 8.3 entry
            let file_type = FileType::try_from(dir_entry.attributes)?;
            let cluster = Cluster::from_high_low(dir_entry.clust_high, dir_entry.clust_low);
            let attr = FileAttr {
                size: dir_entry.size as u64,
                file_type,
                mode: FilePermissions::from_bits_retain(0o755),
                atime: fat_date_to_duration(dir_entry.adate),
                mtime: fat_datetime_to_duration(dir_entry.mdate, dir_entry.mtime, 0),
                ctime: fat_datetime_to_duration(
                    dir_entry.cdate,
                    dir_entry.ctime,
                    dir_entry.ctime_ms,
                ),
                ..Default::default()
            };

            self.lfn_buffer.clear();
            self.offset += 1;

            return Ok(Some(Fat32DirEntry {
                attr,
                cluster,
                name,
                // Note that the offset should be to the *next* entry, so using
                // the advanced entry is correct.
                offset: self.offset,
            }));
        }
    }
}

/// Determines if a given year is a leap year
#[inline(always)]
fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || (year.is_multiple_of(400))
}

/// Returns the number of days in the given month of the specified year.
fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 30, // Fallback (should not happen with valid FAT timestamps)
    }
}

/// Calculates the number of days elapsed since 1980-01-01.
fn days_since_1980(year: u32, mut month: u32, day: u32) -> u32 {
    let mut days = 0;

    // Add whole years.
    for y in 1980..year {
        days += 365 + is_leap_year(y) as u32;
    }

    // Add whole months in the current year.
    while month > 1 {
        month -= 1;
        days += days_in_month(year, month);
    }

    // Add remaining days (day is 1-based, but saturate to avoid underflow for invalid values).
    days + day.saturating_sub(1)
}

/// Converts a 16-bit FAT date field into a `Duration` since the Unix epoch.
fn fat_date_to_duration(date: u16) -> Duration {
    if date == 0 {
        return Duration::ZERO;
    }

    let day = (date & 0b1_1111) as u32;
    let month = ((date >> 5) & 0b1111) as u32;
    let year = ((date >> 9) & 0b111_1111) as u32 + 1980;

    // Guard against obviously invalid encodings (day or month zero).
    if day == 0 || month == 0 {
        return Duration::ZERO;
    }

    // Days between 1970-01-01 and 1980-01-01 = 3652 (incl. 1972 and 1976 leap years)
    const DAYS_OFFSET: u32 = 3652;
    let days_total = DAYS_OFFSET + days_since_1980(year, month, day);

    Duration::from_secs(days_total as u64 * 86_400)
}

/// Converts FAT (date, time, centisecond) fields into a `Duration` since epoch.
fn fat_datetime_to_duration(date: u16, time: u16, csecs: u8) -> Duration {
    // Zero date & time indicates an unset timestamp in FAT; treat as epoch.
    if date == 0 && time == 0 {
        return Duration::ZERO;
    }

    let base = fat_date_to_duration(date);

    let hours = ((time >> 11) & 0b1_1111) as u64;
    let minutes = ((time >> 5) & 0b11_1111) as u64;
    let two_secs = (time & 0b1_1111) as u64;
    let secs = two_secs * 2;

    let millis = (csecs as u64) * 10;

    base + Duration::from_secs(hours * 3600 + minutes * 60 + secs) + Duration::from_millis(millis)
}

#[async_trait]
impl<T: Fat32Operations> DirStream for Fat32DirStream<T> {
    async fn next_entry(&mut self) -> Result<Option<Dirent>> {
        let entry = self.next_fat32_entry().await?;

        Ok(entry.map(|x| Dirent {
            id: InodeId::from_fsid_and_inodeid(self.fs_id, x.cluster.value() as _),
            name: x.name.clone(),
            file_type: x.attr.file_type,
            offset: x.offset,
        }))
    }
}

pub struct Fat32DirNode<T: Fat32Operations> {
    attr: FileAttr,
    root: Cluster,
    fs: Arc<T>,
    streamer: Fat32DirStream<T>,
}

impl<T: Fat32Operations> Fat32DirNode<T> {
    pub fn new(fs: Arc<T>, root: Cluster, attr: FileAttr) -> Self {
        let streamer = Fat32DirStream::new(fs.clone(), root);

        Self {
            attr,
            root,
            fs,
            streamer,
        }
    }
}

#[async_trait]
impl<T: Fat32Operations> Inode for Fat32DirNode<T> {
    fn id(&self) -> InodeId {
        InodeId::from_fsid_and_inodeid(self.fs.id(), self.root.value() as _)
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        let mut dir_iter = self.streamer.clone();

        while let Some(entry) = dir_iter.next_fat32_entry().await? {
            if entry.name.eq_ignore_ascii_case(name) {
                return match entry.attr.file_type {
                    FileType::File => Ok(Arc::new(Fat32FileNode::new(
                        self.fs.clone(),
                        entry.cluster,
                        entry.attr.clone(),
                    )?)),
                    FileType::Directory => Ok(Arc::new(Self::new(
                        self.fs.clone(),
                        entry.cluster,
                        entry.attr.clone(),
                    ))),
                    _ => Err(KernelError::NotSupported),
                };
            }
        }

        Err(FsError::NotFound.into())
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let mut iter = self.streamer.clone();

        iter.advance(start_offset);

        Ok(Box::new(iter))
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr.clone())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::fs::filesystems::fat32::file::test::MockFs;

    mod raw_test;

    fn checksum_83(name: &[u8; 8], ext: &[u8; 3]) -> u8 {
        let mut sum: u8 = 0;
        for &byte in name.iter().chain(ext.iter()) {
            sum = (sum >> 1) | ((sum & 1) << 7); // Rotate right
            sum = sum.wrapping_add(byte);
        }
        sum
    }

    /// A builder to easily create 32-byte DirEntry byte arrays for tests.
    struct DirEntryBuilder {
        entry: DirEntry,
    }

    impl DirEntryBuilder {
        fn new(name: &str, ext: &str) -> Self {
            let mut dos_file_name = [b' '; 8];
            let mut dos_extension = [b' '; 3];
            dos_file_name[..name.len()].copy_from_slice(name.as_bytes());
            dos_extension[..ext.len()].copy_from_slice(ext.as_bytes());

            Self {
                entry: DirEntry {
                    dos_file_name,
                    dos_extension,
                    attributes: Fat32Attributes::ARCHIVE,
                    _reserved: 0,
                    ctime_ms: 0,
                    ctime: 0,
                    cdate: 0,
                    adate: 0,
                    clust_high: 0,
                    mtime: 0,
                    mdate: 0,
                    clust_low: 0,
                    size: 0,
                },
            }
        }

        fn attributes(mut self, attrs: Fat32Attributes) -> Self {
            self.entry.attributes = attrs;
            self
        }

        fn cluster(mut self, cluster: u32) -> Self {
            self.entry.clust_high = (cluster >> 16) as u16;
            self.entry.clust_low = cluster as u16;
            self
        }

        fn size(mut self, size: u32) -> Self {
            self.entry.size = size;
            self
        }

        fn build(self) -> [u8; 32] {
            unsafe { core::mem::transmute(self.entry) }
        }
    }

    /// A builder to create the series of LFN entries for a long name.
    struct LfnBuilder {
        long_name: String,
        checksum: u8,
    }

    impl LfnBuilder {
        fn new(long_name: &str, checksum: u8) -> Self {
            Self {
                long_name: long_name.to_string(),
                checksum,
            }
        }

        fn build(self) -> Vec<[u8; 32]> {
            let mut utf16_chars: Vec<u16> = self.long_name.encode_utf16().collect();
            utf16_chars.push(0x0000); // Null terminator

            // Pad to a multiple of 13 characters
            while utf16_chars.len() % 13 != 0 {
                utf16_chars.push(0xFFFF);
            }

            let mut lfn_entries = Vec::new();
            let num_entries = utf16_chars.len() / 13;

            for i in 0..num_entries {
                let sequence_number = (num_entries - i) as u8;
                let chunk = &utf16_chars[(num_entries - 1 - i) * 13..][..13];

                let mut lfn = LfnEntry {
                    sequence_number,
                    name1: [0; 5],
                    attributes: 0x0F,
                    entry_type: 0,
                    checksum: self.checksum,
                    name2: [0; 6],
                    first_cluster: 0,
                    name3: [0; 2],
                };

                if i == 0 {
                    // First entry on disk is the last logically
                    lfn.sequence_number |= 0x40;
                }

                unsafe {
                    ptr::write_unaligned(&raw mut lfn.name1, chunk[0..5].try_into().unwrap());
                    ptr::write_unaligned(&raw mut lfn.name2, chunk[5..11].try_into().unwrap());
                    ptr::write_unaligned(&raw mut lfn.name3, chunk[11..13].try_into().unwrap());
                }

                lfn_entries.push(unsafe { core::mem::transmute(lfn) });
            }
            lfn_entries
        }
    }

    /// Creates a mock Fat32Filesystem containing the directory data.
    async fn setup_dir_test(dir_data: Vec<u8>) -> Arc<MockFs> {
        Arc::new(MockFs::new(&dir_data, 512, 1))
    }

    async fn collect_entries<T: Fat32Operations>(
        mut iter: Fat32DirStream<T>,
    ) -> Vec<Fat32DirEntry> {
        let mut ret = Vec::new();

        while let Some(entry) = iter.next_fat32_entry().await.unwrap() {
            ret.push(entry);
        }
        ret
    }

    #[tokio::test]
    async fn test_simple_83_entries() {
        let mut data = Vec::new();
        data.extend_from_slice(
            &DirEntryBuilder::new("FILE", "TXT")
                .attributes(Fat32Attributes::ARCHIVE)
                .cluster(10)
                .size(1024)
                .build(),
        );
        data.extend_from_slice(
            &DirEntryBuilder::new("SUBDIR", "")
                .attributes(Fat32Attributes::DIRECTORY)
                .cluster(11)
                .build(),
        );

        let fs = setup_dir_test(data).await;
        let dir_stream = Fat32DirStream::new(fs, Cluster(2));
        let entries = collect_entries(dir_stream).await;

        assert_eq!(entries.len(), 2);

        let file = &entries[0];
        assert_eq!(file.name, "file.txt");
        assert_eq!(file.attr.file_type, FileType::File);
        assert_eq!(file.cluster, Cluster(10));
        assert_eq!(file.attr.size, 1024);

        let dir = &entries[1];
        assert_eq!(dir.name, "subdir");
        assert_eq!(dir.attr.file_type, FileType::Directory);
        assert_eq!(dir.cluster, Cluster(11));
    }

    #[tokio::test]
    async fn test_single_lfn_entry() {
        let sfn = DirEntryBuilder::new("TESTFI~1", "TXT")
            .attributes(Fat32Attributes::ARCHIVE)
            .cluster(5)
            .build();

        let checksum = checksum_83(
            &sfn[0..8].try_into().unwrap(),
            &sfn[8..11].try_into().unwrap(),
        );
        let lfn = LfnBuilder::new("testfile.txt", checksum).build();

        let mut data = Vec::new();
        lfn.into_iter().for_each(|e| data.extend_from_slice(&e));
        data.extend_from_slice(&sfn);

        let fs = setup_dir_test(data).await;
        let dir_stream = Fat32DirStream::new(fs, Cluster(2));
        let entries = collect_entries(dir_stream).await;

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.name, "testfile.txt");
        assert_eq!(entry.cluster, Cluster(5));
        assert_eq!(entry.attr.file_type, FileType::File);
    }

    #[tokio::test]
    async fn test_multi_part_lfn_entry() {
        let sfn = DirEntryBuilder::new("AVERYL~1", "LOG")
            .attributes(Fat32Attributes::ARCHIVE)
            .cluster(42)
            .build();

        let checksum = checksum_83(
            &sfn[0..8].try_into().unwrap(),
            &sfn[8..11].try_into().unwrap(),
        );
        let lfn = LfnBuilder::new("a very long filename indeed.log", checksum).build();

        let mut data = Vec::new();
        lfn.into_iter().for_each(|e| data.extend_from_slice(&e));
        data.extend_from_slice(&sfn);

        let fs = setup_dir_test(data).await;
        let dir_stream = Fat32DirStream::new(fs, Cluster(2));
        let entries = collect_entries(dir_stream).await;

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.name, "a very long filename indeed.log");
        assert_eq!(entry.cluster, Cluster(42));
    }

    #[tokio::test]
    async fn ignores_deleted_entries() {
        let mut data = Vec::new();
        let mut deleted_entry = DirEntryBuilder::new("DELETED", "TMP").build();
        deleted_entry[0] = 0xE5; // Mark as deleted

        data.extend_from_slice(&deleted_entry);
        data.extend_from_slice(&DirEntryBuilder::new("GOODFILE", "DAT").build());

        let fs = setup_dir_test(data).await;
        let dir_stream = Fat32DirStream::new(fs, Cluster(2));
        let entries = collect_entries(dir_stream).await;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "goodfile.dat");
        assert_eq!(entries[0].offset, 2);
    }

    #[tokio::test]
    async fn stops_at_end_of_dir_marker() {
        let mut data = Vec::new();
        data.extend_from_slice(&DirEntryBuilder::new("FIRST", "FIL").build());
        data.extend_from_slice(&[0u8; 32]); // End of directory marker
        data.extend_from_slice(&DirEntryBuilder::new("JUNK", "FIL").build()); // Should not be parsed

        let fs = setup_dir_test(data).await;
        let dir_stream = Fat32DirStream::new(fs, Cluster(2));
        let entries = collect_entries(dir_stream).await;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "first.fil");
    }

    #[tokio::test]
    async fn test_ignores_volume_label() {
        let mut data = Vec::new();
        data.extend_from_slice(
            &DirEntryBuilder::new("MYVOLUME", "")
                .attributes(Fat32Attributes::VOLUME_LABEL)
                .build(),
        );
        data.extend_from_slice(&DirEntryBuilder::new("REALFILE", "TXT").build());

        let fs = setup_dir_test(data).await;
        let dir_stream = Fat32DirStream::new(fs, Cluster(2));
        let entries = collect_entries(dir_stream).await;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "realfile.txt");
        assert_eq!(entries[0].offset, 2);
    }

    #[tokio::test]
    async fn raw() {
        let mut data = Vec::new();
        data.extend_from_slice(&raw_test::RAW_DATA);

        let fs = setup_dir_test(data).await;
        let dir_stream = Fat32DirStream::new(fs, Cluster(2));
        let entries = collect_entries(dir_stream).await;

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "test.txt");
        assert_eq!(entries[0].cluster, Cluster(0xb));
        assert_eq!(entries[0].attr.size, 0xd);
        assert_eq!(
            entries[1].name,
            "some-really-long-file-name-that-should-span-over-multiple-entries.txt"
        );
        assert_eq!(entries[1].cluster, Cluster(0));
        assert_eq!(entries[1].attr.size, 0);
    }

    #[tokio::test]
    async fn test_mixed_directory_listing() {
        let mut data = Vec::new();

        // 1. A standard 8.3 file
        data.extend_from_slice(
            &DirEntryBuilder::new("KERNEL", "ELF")
                .attributes(Fat32Attributes::ARCHIVE)
                .cluster(3)
                .build(),
        );

        // 2. A deleted LFN entry
        let deleted_sfn = DirEntryBuilder::new("OLDLOG~1", "TMP").build();
        let deleted_checksum = checksum_83(
            &deleted_sfn[0..8].try_into().unwrap(),
            &deleted_sfn[8..11].try_into().unwrap(),
        );
        let mut deleted_lfn_bytes = LfnBuilder::new("old-log-file.tmp", deleted_checksum)
            .build()
            .remove(0);
        deleted_lfn_bytes[0] = 0xE5;
        data.extend_from_slice(&deleted_lfn_bytes);

        // 3. A valid LFN file
        let sfn = DirEntryBuilder::new("MYNOTE~1", "MD")
            .attributes(Fat32Attributes::ARCHIVE)
            .cluster(4)
            .build();
        let checksum = checksum_83(
            &sfn[0..8].try_into().unwrap(),
            &sfn[8..11].try_into().unwrap(),
        );
        let lfn = LfnBuilder::new("my notes.md", checksum).build();
        lfn.into_iter().for_each(|e| data.extend_from_slice(&e));
        data.extend_from_slice(&sfn);

        let fs = setup_dir_test(data).await;
        let dir_stream = Fat32DirStream::new(fs, Cluster(2));
        let entries = collect_entries(dir_stream).await;

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "kernel.elf");
        assert_eq!(entries[0].cluster, Cluster(3));
        assert_eq!(entries[1].name, "my notes.md");
        assert_eq!(entries[1].cluster, Cluster(4));
    }
}
