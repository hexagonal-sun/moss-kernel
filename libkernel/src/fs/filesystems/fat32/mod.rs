use crate::{
    CpuOps,
    error::{FsError, Result},
    fs::{FileType, Filesystem, Inode, InodeId, attr::FileAttr, blk::buffer::BlockBuffer},
};
use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use async_trait::async_trait;
use bpb::BiosParameterBlock;
use core::{
    cmp::min,
    fmt::Display,
    ops::{Add, Mul},
};
use dir::Fat32DirNode;
use fat::Fat;
use log::warn;

mod bpb;
mod dir;
mod fat;
mod file;
mod reader;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sector(u32);

impl Mul<usize> for Sector {
    type Output = Sector;

    fn mul(self, rhs: usize) -> Self::Output {
        Self(self.0 * rhs as u32)
    }
}

impl Add<Sector> for Sector {
    type Output = Sector;

    fn add(self, rhs: Sector) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sector {
    pub fn sectors_until(self, other: Self) -> impl Iterator<Item = Self> {
        (self.0..other.0).map(Sector)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Cluster(u32);

impl Cluster {
    pub fn value(self) -> usize {
        self.0 as _
    }

    pub fn from_high_low(clust_high: u16, clust_low: u16) -> Cluster {
        Cluster((clust_high as u32) << 16 | clust_low as u32)
    }

    pub fn is_valid(self) -> bool {
        self.0 >= 2
    }
}

impl Display for Cluster {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

pub struct Fat32Filesystem<CPU>
where
    CPU: CpuOps,
{
    dev: BlockBuffer<CPU>,
    bpb: BiosParameterBlock,
    fat: Fat,
    id: u64,
    this: Weak<Self>,
}

impl<CPU> Fat32Filesystem<CPU>
where
    CPU: CpuOps,
{
    pub async fn new(dev: BlockBuffer<CPU>, id: u64) -> Result<Arc<Self>> {
        let bpb = BiosParameterBlock::new(&dev).await?;
        let fat = Fat::read_fat(&dev, &bpb, 0).await?;

        for fat_num in 1..bpb.num_fats {
            let other_fat = Fat::read_fat(&dev, &bpb, fat_num as _).await?;

            if other_fat != fat {
                warn!("Failing to mount, FAT disagree.");
                return Err(FsError::InvalidFs.into());
            }
        }

        Ok(Arc::new_cyclic(|weak| Self {
            bpb,
            dev,
            fat,
            this: weak.clone(),
            id,
        }))
    }
}

trait Fat32Operations: Send + Sync + 'static {
    fn read_sector(
        &self,
        sector: Sector,
        offset: usize,
        buf: &mut [u8],
    ) -> impl Future<Output = Result<usize>> + Send;

    fn id(&self) -> u64;
    fn sector_size(&self) -> usize;
    fn sectors_per_cluster(&self) -> usize;

    fn bytes_per_cluster(&self) -> usize {
        self.sectors_per_cluster() * self.sector_size()
    }

    fn cluster_to_sectors(&self, cluster: Cluster) -> Result<impl Iterator<Item = Sector> + Send>;
    fn iter_clusters(&self, root: Cluster) -> impl Iterator<Item = Result<Cluster>> + Send;
}

impl<CPU> Fat32Operations for Fat32Filesystem<CPU>
where
    CPU: CpuOps,
{
    async fn read_sector(&self, sector: Sector, offset: usize, buf: &mut [u8]) -> Result<usize> {
        debug_assert!(offset < self.bpb.sector_size());

        let bytes_left_in_sec = self.bpb.sector_size() - offset;

        let read_sz = min(buf.len(), bytes_left_in_sec);

        self.dev
            .read_at(
                self.bpb.sector_offset(sector) + offset as u64,
                &mut buf[..read_sz],
            )
            .await?;

        Ok(read_sz)
    }

    fn id(&self) -> u64 {
        self.id
    }

    fn sector_size(&self) -> usize {
        self.bpb.sector_size()
    }

    fn sectors_per_cluster(&self) -> usize {
        self.bpb.sectors_per_cluster as _
    }

    fn cluster_to_sectors(&self, cluster: Cluster) -> Result<impl Iterator<Item = Sector>> {
        self.bpb.cluster_to_sectors(cluster)
    }

    fn iter_clusters(&self, root: Cluster) -> impl Iterator<Item = Result<Cluster>> {
        self.fat.get_cluster_chain(root)
    }
}

#[async_trait]
impl<CPU> Filesystem for Fat32Filesystem<CPU>
where
    CPU: CpuOps,
{
    fn id(&self) -> u64 {
        self.id
    }

    /// Get the root inode of this filesystem.
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        Ok(Arc::new(Fat32DirNode::new(
            self.this.upgrade().unwrap(),
            self.bpb.root_cluster,
            FileAttr {
                id: InodeId::from_fsid_and_inodeid(self.id, self.bpb.root_cluster.0 as _),
                file_type: FileType::Directory,
                ..FileAttr::default()
            },
        )))
    }
}
