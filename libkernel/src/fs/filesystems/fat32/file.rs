use crate::{
    error::Result,
    fs::{Inode, InodeId, attr::FileAttr},
};
use alloc::boxed::Box;
use alloc::sync::Arc;
use async_trait::async_trait;
use core::any::Any;

use super::{Cluster, Fat32Operations, reader::Fat32Reader};

pub struct Fat32FileNode<T: Fat32Operations> {
    reader: Fat32Reader<T>,
    attr: FileAttr,
    id: InodeId,
}

impl<T: Fat32Operations> Fat32FileNode<T> {
    pub fn new(fs: Arc<T>, root: Cluster, attr: FileAttr) -> Result<Self> {
        let id = InodeId::from_fsid_and_inodeid(fs.id() as _, root.value() as _);

        Ok(Self {
            reader: Fat32Reader::new(fs, root, attr.size),
            attr,
            id,
        })
    }
}

#[async_trait]
impl<T: Fat32Operations> Inode for Fat32FileNode<T> {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        self.reader.read_at(offset, buf).await
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
pub mod test {
    use crate::{error::FsError, fs::filesystems::fat32::Sector};

    use super::*;
    use alloc::{collections::BTreeMap, sync::Arc, vec};

    pub struct MockFs {
        file_data: BTreeMap<u32, Vec<u8>>, // Map Sector(u32) -> data
        sector_size: usize,
        sectors_per_cluster: usize,
    }

    impl MockFs {
        pub fn new(file_content: &[u8], sector_size: usize, sectors_per_cluster: usize) -> Self {
            let mut file_data = BTreeMap::new();
            // Data region starts at sector 100 for simplicity
            let data_start_sector = 100;

            for (i, chunk) in file_content.chunks(sector_size).enumerate() {
                let mut sector_data = vec![0; sector_size];
                sector_data[..chunk.len()].copy_from_slice(chunk);
                file_data.insert((data_start_sector + i) as u32, sector_data);
            }

            Self {
                file_data,
                sector_size,
                sectors_per_cluster,
            }
        }
    }

    impl Fat32Operations for MockFs {
        async fn read_sector(
            &self,
            sector: Sector,
            offset: usize,
            buf: &mut [u8],
        ) -> Result<usize> {
            let sector_data = self.file_data.get(&sector.0).ok_or(FsError::OutOfBounds)?;
            let bytes_in_sec = sector_data.len() - offset;
            let read_size = core::cmp::min(buf.len(), bytes_in_sec);
            buf[..read_size].copy_from_slice(&sector_data[offset..offset + read_size]);
            Ok(read_size)
        }

        fn id(&self) -> u64 {
            0
        }
        fn sector_size(&self) -> usize {
            self.sector_size
        }
        fn sectors_per_cluster(&self) -> usize {
            self.sectors_per_cluster
        }
        fn bytes_per_cluster(&self) -> usize {
            self.sector_size * self.sectors_per_cluster
        }

        fn cluster_to_sectors(&self, cluster: Cluster) -> Result<impl Iterator<Item = Sector>> {
            // Simple mapping for the test: Cluster C -> Sectors (100 + (C-2)*SPC ..)
            let data_start_sector = 100;
            let start = data_start_sector + (cluster.value() - 2) * self.sectors_per_cluster;
            let end = start + self.sectors_per_cluster;
            Ok((start as u32..end as u32).map(Sector))
        }

        fn iter_clusters(&self, root: Cluster) -> impl Iterator<Item = Result<Cluster>> {
            // Assume a simple contiguous chain for testing.
            let num_clusters =
                (self.file_data.len() + self.sectors_per_cluster - 1) / self.sectors_per_cluster;
            (0..num_clusters).map(move |i| Ok(Cluster((root.value() + i) as u32)))
        }
    }

    async fn setup_file_test(content: &[u8]) -> Fat32FileNode<MockFs> {
        let fs = Arc::new(MockFs::new(content, 512, 4));
        Fat32FileNode::new(
            fs,
            Cluster(2),
            FileAttr {
                size: content.len() as _,
                ..FileAttr::default()
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn test_read_simple() {
        let file_content: Vec<u8> = (0..100).collect();
        let inode = setup_file_test(&file_content).await;

        let mut buf = vec![0; 50];
        let bytes_read = inode.read_at(10, &mut buf).await.unwrap();

        assert_eq!(bytes_read, 50);
        assert_eq!(buf, &file_content[10..60]);
    }

    #[tokio::test]
    async fn test_read_crossing_sector_boundary() {
        let file_content: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
        let inode = setup_file_test(&file_content).await;

        // Read from offset 510 for 4 bytes. Should read 2 bytes from sector 0
        // and 2 bytes from sector 1.
        let mut buf = vec![0; 4];
        let bytes_read = inode.read_at(510, &mut buf).await.unwrap();

        assert_eq!(bytes_read, 4);
        assert_eq!(buf, &file_content[510..514]);
    }

    #[tokio::test]
    async fn test_read_crossing_cluster_boundary() {
        // Sector size = 512, Sectors per cluster = 4 -> Cluster size = 2048
        let file_content: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();
        let inode = setup_file_test(&file_content).await;

        // Read from offset 2040 for 16 bytes. Should cross from cluster 2 to cluster 3.
        let mut buf = vec![0; 16];
        let bytes_read = inode.read_at(2040, &mut buf).await.unwrap();

        assert_eq!(bytes_read, 16);
        assert_eq!(buf, &file_content[2040..2056]);
    }

    #[tokio::test]
    async fn test_read_past_eof() {
        let file_content: Vec<u8> = (0..100).collect();
        let inode = setup_file_test(&file_content).await;

        let mut buf = vec![0; 50];
        // Start reading at offset 80, but buffer is 50. Should only read 20 bytes.
        let bytes_read = inode.read_at(80, &mut buf).await.unwrap();

        assert_eq!(bytes_read, 20);
        assert_eq!(buf[..20], file_content[80..100]);
    }

    #[tokio::test]
    async fn test_read_at_eof() {
        let file_content: Vec<u8> = (0..100).collect();
        let inode = setup_file_test(&file_content).await;

        let mut buf = vec![0; 50];
        let bytes_read = inode.read_at(100, &mut buf).await.unwrap();

        assert_eq!(bytes_read, 0);
    }
}
