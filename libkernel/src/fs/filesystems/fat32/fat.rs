use crate::{
    CpuOps,
    error::{FsError, IoError, Result},
    fs::blk::buffer::BlockBuffer,
};

use alloc::vec;
use alloc::vec::Vec;

use super::{Cluster, bpb::BiosParameterBlock};

#[derive(PartialEq, Eq, Debug)]
pub enum FatEntry {
    Eoc,
    NextCluster(Cluster),
    Bad,
    Reserved,
    Free,
}

impl From<u32> for FatEntry {
    fn from(value: u32) -> Self {
        match value & 0x0fffffff {
            0 => Self::Free,
            1 => Self::Reserved,
            n @ 2..=0xFFFFFF6 => Self::NextCluster(Cluster(n)),
            0xFFFFFF7 => Self::Bad,
            0xFFFFFF8..=0xFFFFFFF => Self::Eoc,
            _ => unreachable!("The last nibble has been masked"),
        }
    }
}

#[derive(PartialEq, Eq, Debug)]
pub struct Fat {
    data: Vec<FatEntry>,
}

pub struct ClusterChainIterator<'a> {
    fat: &'a Fat,
    current_or_next: Option<Cluster>,
}

impl<'a> Iterator for ClusterChainIterator<'a> {
    type Item = Result<Cluster>;

    fn next(&mut self) -> Option<Self::Item> {
        let cluster_to_return = self.current_or_next?;

        let entry = match self.fat.data.get(cluster_to_return.value()) {
            Some(entry) => entry,
            None => {
                self.current_or_next = None;
                return Some(Err(IoError::OutOfBounds.into()));
            }
        };

        match entry {
            FatEntry::Eoc => {
                self.current_or_next = None;
            }
            FatEntry::NextCluster(next) => {
                self.current_or_next = Some(*next);
            }
            FatEntry::Bad | FatEntry::Reserved | FatEntry::Free => {
                self.current_or_next = None;
                return Some(Err(IoError::MetadataCorruption.into()));
            }
        }

        Some(Ok(cluster_to_return))
    }
}

impl Fat {
    pub async fn read_fat<CPU>(
        dev: &BlockBuffer<CPU>,
        bpb: &BiosParameterBlock,
        fat_number: usize,
    ) -> Result<Self>
    where
        CPU: CpuOps,
    {
        let (start, end) = bpb.fat_region(fat_number).ok_or(FsError::InvalidFs)?;

        let mut fat: Vec<FatEntry> = Vec::with_capacity(
            (bpb.sector_offset(end) as usize - bpb.sector_offset(start) as usize) / 4,
        );

        let mut buf = vec![0; bpb.sector_size()];

        for sec in start.sectors_until(end) {
            dev.read_at(bpb.sector_offset(sec), &mut buf).await?;

            fat.extend(
                buf.chunks_exact(4)
                    .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
                    .map(|v| v.into()),
            );
        }

        Ok(Self { data: fat })
    }

    pub fn get_cluster_chain(&self, root: Cluster) -> impl Iterator<Item = Result<Cluster>> {
        ClusterChainIterator {
            fat: self,
            current_or_next: Some(root),
        }
    }
}

#[cfg(test)]
mod test {
    use crate::error::{IoError, KernelError, Result};
    use crate::fs::filesystems::fat32::Cluster;
    use crate::fs::filesystems::fat32::bpb::test::create_test_bpb;
    use crate::fs::filesystems::fat32::fat::{Fat, FatEntry};
    use crate::fs::{BlockDevice, blk::buffer::BlockBuffer};
    use crate::test::MockCpuOps;
    use async_trait::async_trait;

    const EOC: u32 = 0xFFFFFFFF;
    const BAD: u32 = 0xFFFFFFF7;
    const FREE: u32 = 0;
    const RESERVED: u32 = 1;

    struct MemBlkDevice {
        data: Vec<u8>,
    }

    #[async_trait]
    impl BlockDevice for MemBlkDevice {
        /// Read one or more blocks starting at `block_id`.
        /// The `buf` length must be a multiple of `block_size`.
        async fn read(&self, block_id: u64, buf: &mut [u8]) -> Result<()> {
            buf.copy_from_slice(&self.data[block_id as usize..block_id as usize + buf.len()]);
            Ok(())
        }

        /// Write one or more blocks starting at `block_id`.
        /// The `buf` length must be a multiple of `block_size`.
        async fn write(&self, _block_id: u64, _buf: &[u8]) -> Result<()> {
            unimplemented!()
        }

        /// The size of a single block in bytes.
        fn block_size(&self) -> usize {
            1
        }

        /// Flushes any caches to the underlying device.
        async fn sync(&self) -> Result<()> {
            unimplemented!()
        }
    }

    fn setup_fat_test(fat_data: &[u32]) -> BlockBuffer<MockCpuOps> {
        let mut data = Vec::new();
        data.extend(fat_data.iter().flat_map(|x| x.to_le_bytes()));

        BlockBuffer::new(Box::new(MemBlkDevice { data }))
    }

    #[tokio::test]
    async fn test_read_fat_simple_parse() {
        let fat_data = [
            FREE,                    // Cluster 0
            RESERVED,                // Cluster 1
            EOC,                     // Cluster 2
            5,                       // Cluster 3 -> 5
            BAD,                     // Cluster 4
            EOC,                     // Cluster 5
            0xDEADBEEF & 0x0FFFFFFF, // Test masking of top bits
        ];

        let device = setup_fat_test(&fat_data);
        let mut bpb = create_test_bpb();
        bpb.bytes_per_sector = fat_data.len() as u16 * 4;
        bpb.sectors_per_cluster = 1;
        bpb.num_fats = 1;
        bpb.fat_size_32 = 1;
        bpb.reserved_sector_count = 0;

        let fat = Fat::read_fat(&device, &bpb, 0)
            .await
            .expect("read_fat should succeed");

        assert_eq!(
            fat.data.len(),
            fat_data.len(),
            "Parsed FAT has incorrect length"
        );
        assert_eq!(fat.data[0], FatEntry::Free);
        assert_eq!(fat.data[1], FatEntry::Reserved);
        assert_eq!(fat.data[2], FatEntry::Eoc);
        assert_eq!(fat.data[3], FatEntry::NextCluster(Cluster(5)));
        assert_eq!(fat.data[4], FatEntry::Bad);
        assert_eq!(fat.data[5], FatEntry::Eoc);
        // Ensure the top 4 bits are ignored.
        assert_eq!(fat.data[6], FatEntry::NextCluster(Cluster(0x0EADBEEF)));
    }

    #[tokio::test]
    async fn test_read_fat_across_multiple_sectors() {
        // A sector size of 512 bytes can hold 128 u32 entries.
        // We'll create a FAT that is slightly larger to force a multi-sector read.
        let mut fat_data = Vec::with_capacity(150);
        for i in 0..150 {
            fat_data.push(i + 2); // Create a simple chain: 0->2, 1->3, etc.
        }
        fat_data[149] = 0xFFFFFFFF; // End the last chain

        let device = setup_fat_test(&fat_data);
        let mut bpb = create_test_bpb();
        bpb.bytes_per_sector = 300;
        bpb.num_fats = 1;
        bpb.reserved_sector_count = 0;
        bpb.sectors_per_cluster = 1;
        bpb.fat_size_32 = 2;

        let fat = super::Fat::read_fat(&device, &bpb, 0)
            .await
            .expect("read_fat should succeed");

        assert!(super::Fat::read_fat(&device, &bpb, 1).await.is_err());

        assert_eq!(fat.data.len(), 150, "Parsed FAT has incorrect length");
        assert_eq!(fat.data[0], FatEntry::NextCluster(Cluster(2)));
        assert_eq!(fat.data[127], FatEntry::NextCluster(Cluster(129))); // End of 1st sector
        assert_eq!(fat.data[128], FatEntry::NextCluster(Cluster(130))); // Start of 2nd sector
        assert_eq!(fat.data[149], FatEntry::Eoc);
    }

    fn setup_chain_test_fat() -> super::Fat {
        #[rustfmt::skip]
        let fat_data = [
            /* 0  */ FREE,
            /* 1  */ RESERVED,
            /* 2  */ EOC, // Single-cluster file
            /* 3  */ 4, // Start of linear chain
            /* 4  */ 5,
            /* 5  */ EOC,
            /* 6  */ 10, // Start of fragmented chain
            /* 7  */ 9, // Chain leading to a bad cluster
            /* 8  */ EOC,
            /* 9  */ BAD,
            /* 10 */ 8,
            /* 11 */ 12, // Chain leading to a free cluster
            /* 12 */ FREE,
            /* 13 */ 14, // Chain with a cycle
            /* 14 */ 15,
            /* 15 */ 13,
            /* 16 */ 99, // Chain pointing out of bounds
        ];

        let data = fat_data.iter().map(|&v| FatEntry::from(v)).collect();
        Fat { data }
    }

    #[test]
    fn test_chain_single_cluster() {
        let fat = setup_chain_test_fat();
        let chain: Vec<_> = fat.get_cluster_chain(Cluster(2)).collect();
        assert_eq!(chain, vec![Ok(Cluster(2))]);
    }

    #[test]
    fn test_chain_linear() {
        let fat = setup_chain_test_fat();
        let chain: Vec<_> = fat.get_cluster_chain(Cluster(3)).collect();
        assert_eq!(chain, vec![Ok(Cluster(3)), Ok(Cluster(4)), Ok(Cluster(5))]);
    }

    #[test]
    fn test_chain_fragmented() {
        let fat = setup_chain_test_fat();
        let chain: Vec<_> = fat.get_cluster_chain(Cluster(6)).collect();
        assert_eq!(chain, vec![Ok(Cluster(6)), Ok(Cluster(10)), Ok(Cluster(8))]);
    }

    #[test]
    fn test_chain_points_to_bad_cluster() {
        let fat = setup_chain_test_fat();
        let chain: Vec<_> = fat.get_cluster_chain(Cluster(7)).collect();
        assert_eq!(chain.len(), 2);
        assert!(
            chain[1].is_err(),
            "Should fail when chain encounters a bad cluster"
        );
        assert!(matches!(
            chain[1],
            Err(KernelError::Io(IoError::MetadataCorruption))
        ));
    }

    #[test]
    fn test_chain_points_to_free_cluster() {
        let fat = setup_chain_test_fat();
        let chain: Vec<_> = fat.get_cluster_chain(Cluster(11)).collect();
        assert_eq!(chain.len(), 2);
        assert!(
            chain[1].is_err(),
            "Should fail when chain encounters a free cluster"
        );
        assert!(matches!(
            chain[1],
            Err(KernelError::Io(IoError::MetadataCorruption))
        ));
    }

    #[test]
    fn test_chain_points_out_of_bounds() {
        let fat = setup_chain_test_fat();
        let result: Vec<_> = fat.get_cluster_chain(Cluster(16)).collect();
        dbg!(&result);
        assert_eq!(result.len(), 2);

        assert!(
            result[1].is_err(),
            "Should fail when chain points to an out-of-bounds cluster"
        );
        assert!(matches!(
            result[1],
            Err(KernelError::Io(IoError::OutOfBounds))
        ));
    }

    #[test]
    fn test_chain_starts_out_of_bounds() {
        let fat = setup_chain_test_fat();
        // Start with a cluster number that is larger than the FAT itself.
        let chain: Vec<_> = fat.get_cluster_chain(Cluster(100)).collect();
        assert!(
            chain[0].is_err(),
            "Should fail when the starting cluster is out-of-bounds"
        );
        assert!(matches!(
            chain[0],
            Err(KernelError::Io(IoError::OutOfBounds))
        ));
    }
}
