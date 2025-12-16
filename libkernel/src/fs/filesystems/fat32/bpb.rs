use crate::{
    error::{FsError, Result},
    fs::blk::buffer::BlockBuffer,
    pod::Pod,
};
use log::warn;

use super::{Cluster, Sector};

#[repr(C, packed)]
#[derive(Debug)]
pub struct BiosParameterBlock {
    _jump: [u8; 3],
    _oem_id: [u8; 8],

    /* DOS 2.0 BPB */
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    // Number of sectors in the Reserved Region. Usually 32.
    pub reserved_sector_count: u16,
    pub num_fats: u8,
    _root_entry_count: u16,
    _total_sectors_16: u16,
    _media_type: u8,
    _fat_size_16: u16,
    _sectors_per_track: u16,
    _head_count: u16,
    _hidden_sector_count: u32,
    _total_sectors_32: u32,

    /* FAT32 Extended BPB */
    // The size of ONE FAT in sectors.
    pub fat_size_32: u32,
    _ext_flags: u16,
    _fs_version: u16,
    // The cluster number where the root directory starts.
    pub root_cluster: Cluster,
    pub fsinfo_sector: u16,
    // More stuff.  Ignored, for now.
}

unsafe impl Pod for BiosParameterBlock {}

impl BiosParameterBlock {
    pub async fn new(dev: &BlockBuffer) -> Result<Self> {
        let bpb: Self = dev.read_obj(0).await?;

        if bpb._fat_size_16 != 0 || bpb._root_entry_count != 0 {
            warn!("Not a FAT32 volume (FAT16 fields are non-zero)");
            return Err(FsError::InvalidFs.into());
        }

        if bpb.fat_size_32 == 0 {
            warn!("FAT32 size is zero");
            return Err(FsError::InvalidFs.into());
        }

        if bpb.num_fats == 0 {
            warn!("Volume has 0 FATs, which is invalid.");
            return Err(FsError::InvalidFs.into());
        }

        let bytes_per_sector = bpb.bytes_per_sector;
        match bytes_per_sector {
            512 | 1024 | 2048 | 4096 => {} // Good!
            _ => {
                warn!(
                    "Bytes per sector {} is not a valid value (must be 512, 1024, 2048, or 4096).",
                    bytes_per_sector
                );
                return Err(FsError::InvalidFs.into());
            }
        }

        if !bpb.bytes_per_sector.is_power_of_two() {
            let bytes_per_sector = bpb.bytes_per_sector;

            warn!(
                "Bytes per sector 0x{:X} not a power of two.",
                bytes_per_sector
            );
            return Err(FsError::InvalidFs.into());
        }

        if !bpb.sectors_per_cluster.is_power_of_two() {
            warn!(
                "Sectors per cluster 0x{:X} not a power of two.",
                bpb.sectors_per_cluster
            );
            return Err(FsError::InvalidFs.into());
        }

        if !bpb.root_cluster.is_valid() {
            let root_cluster = bpb.root_cluster;

            warn!("Root cluster {} < 2.", root_cluster);

            return Err(FsError::InvalidFs.into());
        }

        Ok(bpb)
    }

    pub fn sector_offset(&self, sector: Sector) -> u64 {
        sector.0 as u64 * self.bytes_per_sector as u64
    }

    pub fn fat_region(&self, fat_number: usize) -> Option<(Sector, Sector)> {
        if fat_number >= self.num_fats as usize {
            None
        } else {
            let start = self.fat_region_start() + self.fat_len() * fat_number;
            let end = start + self.fat_len();

            Some((start, end))
        }
    }

    pub fn fat_region_start(&self) -> Sector {
        Sector(self.reserved_sector_count as _)
    }

    pub fn fat_len(&self) -> Sector {
        Sector(self.fat_size_32 as _)
    }

    pub fn data_region_start(&self) -> Sector {
        self.fat_region_start() + self.fat_len() * self.num_fats as usize
    }

    pub fn sector_size(&self) -> usize {
        self.bytes_per_sector as _
    }

    pub fn cluster_to_sectors(&self, cluster: Cluster) -> Result<impl Iterator<Item = Sector>> {
        if cluster.0 < 2 {
            warn!("Cannot conver sentinel cluster number");
            Err(FsError::InvalidFs.into())
        } else {
            let root_sector = Sector(
                self.data_region_start().0 + (cluster.0 - 2) * self.sectors_per_cluster as u32,
            );

            Ok(root_sector.sectors_until(Sector(root_sector.0 + self.sectors_per_cluster as u32)))
        }
    }
}

#[cfg(test)]
pub mod test {
    use super::{BiosParameterBlock, Cluster, Sector};

    // A helper to create a typical FAT32 BPB for testing.
    pub fn create_test_bpb() -> BiosParameterBlock {
        BiosParameterBlock {
            _jump: [0; 3],
            _oem_id: [0; 8],
            bytes_per_sector: 512,
            sectors_per_cluster: 8,
            reserved_sector_count: 32,
            num_fats: 2,
            _root_entry_count: 0,
            _total_sectors_16: 0,
            _media_type: 0,
            _fat_size_16: 0,
            _sectors_per_track: 0,
            _head_count: 0,
            _hidden_sector_count: 0,
            _total_sectors_32: 0,
            fat_size_32: 1000, // Size of ONE FAT in sectors
            _ext_flags: 0,
            _fs_version: 0,
            root_cluster: Cluster(2),
            fsinfo_sector: 0,
        }
    }

    #[test]
    fn sector_iter() {
        let sec = Sector(3);
        let mut iter = sec.sectors_until(Sector(6));

        assert_eq!(iter.next(), Some(Sector(3)));
        assert_eq!(iter.next(), Some(Sector(4)));
        assert_eq!(iter.next(), Some(Sector(5)));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn cluster_validity() {
        assert!(!Cluster(0).is_valid());
        assert!(!Cluster(1).is_valid());
        assert!(Cluster(2).is_valid());
        assert!(Cluster(u32::MAX).is_valid());
    }

    #[test]
    fn fat_layout_calculations() {
        let bpb = create_test_bpb();

        // The first FAT should start immediately after the reserved region.
        assert_eq!(bpb.fat_region_start(), Sector(32));

        assert_eq!(bpb.fat_len(), Sector(1000));

        // 32 (reserved) + 2 * 1000 (fats) = 2032
        assert_eq!(bpb.data_region_start(), Sector(2032));
    }

    #[test]
    fn fat_region_lookup() {
        let bpb = create_test_bpb();

        // First FAT (FAT 0)
        let (start0, end0) = bpb.fat_region(0).unwrap();
        assert_eq!(start0, Sector(32));
        assert_eq!(end0, Sector(32 + 1000));

        // Second FAT (FAT 1)
        let (start1, end1) = bpb.fat_region(1).unwrap();
        assert_eq!(start1, Sector(32 + 1000));
        assert_eq!(end1, Sector(32 + 1000 + 1000));

        // There is no third FAT
        assert!(bpb.fat_region(2).is_none());
    }

    #[test]
    fn cluster_to_sector_conversion() {
        let bpb = create_test_bpb();
        let data_start = bpb.data_region_start(); // 2032
        let spc = bpb.sectors_per_cluster as u32; // 8

        // Cluster 2 is the first data cluster. It should start at the beginning
        // of the data region.
        let mut cluster2_sectors = bpb.cluster_to_sectors(Cluster(2)).unwrap();
        assert_eq!(cluster2_sectors.next(), Some(Sector(data_start.0))); // 2032
        assert_eq!(
            cluster2_sectors.last(),
            Some(Sector(data_start.0 + spc - 1))
        ); // 2039

        // Cluster 3 is the second data cluster.
        let mut cluster3_sectors = bpb.cluster_to_sectors(Cluster(3)).unwrap();
        assert_eq!(cluster3_sectors.next(), Some(Sector(data_start.0 + spc))); // 2040
        assert_eq!(
            cluster3_sectors.last(),
            Some(Sector(data_start.0 + 2 * spc - 1))
        ); // 2047
    }

    #[test]
    fn cluster_to_sector_invalid_input() {
        let bpb = create_test_bpb();

        // Clusters 0 and 1 are reserved and should not be converted to sectors.
        assert!(matches!(bpb.cluster_to_sectors(Cluster(0)), Err(_)));
        assert!(matches!(bpb.cluster_to_sectors(Cluster(1)), Err(_)));
    }
}
