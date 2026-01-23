use crate::error::Result;
use alloc::sync::Arc;

use super::{Cluster, Fat32Operations};

pub struct Fat32Reader<T: Fat32Operations> {
    fs: Arc<T>,
    root: Cluster,
    max_sz: u64,
}

impl<T: Fat32Operations> Clone for Fat32Reader<T> {
    fn clone(&self) -> Self {
        Self {
            fs: self.fs.clone(),
            root: self.root,
            max_sz: self.max_sz,
        }
    }
}

impl<T: Fat32Operations> Fat32Reader<T> {
    pub fn new(fs: Arc<T>, root: Cluster, max_sz: u64) -> Self {
        Self { fs, root, max_sz }
    }

    pub async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        // Ensure we don't read past the end of the stream.
        if offset >= self.max_sz {
            return Ok(0);
        }

        let bytes_to_read = core::cmp::min(buf.len() as u64, self.max_sz - offset) as usize;

        if bytes_to_read == 0 {
            return Ok(0);
        }

        let buf = &mut buf[..bytes_to_read];
        let mut total_bytes_read = 0;

        let bpc = self.fs.bytes_per_cluster();
        let sector_size = self.fs.sector_size();

        // Determine the maximum possible length of the cluster chain from the
        // file size. This acts as a safety rail against cycles in the FAT.
        let max_clusters = self.max_sz.div_ceil(bpc as _);

        // Calculate the starting position.
        let start_cluster_idx = (offset / bpc as u64) as usize;
        let offset_in_first_cluster = (offset % bpc as u64) as usize;
        let start_sector_idx_in_cluster = offset_in_first_cluster / sector_size;
        let offset_in_first_sector = offset_in_first_cluster % sector_size;

        // Get the cluster iterator and advance it to our starting cluster.
        let mut cluster_iter = self.fs.iter_clusters(self.root).take(max_clusters as _);

        if let Some(cluster_result) = cluster_iter.nth(start_cluster_idx) {
            let cluster = cluster_result?;
            let mut sectors = self.fs.cluster_to_sectors(cluster)?;

            // Advance the sector iterator to the correct starting sector.
            if let Some(sector) = sectors.nth(start_sector_idx_in_cluster) {
                // Read the first, possibly partial, chunk from the first sector.
                let bytes_read = self
                    .fs
                    .read_sector(sector, offset_in_first_sector, &mut buf[total_bytes_read..])
                    .await?;
                total_bytes_read += bytes_read;

                // Read any remaining full sectors within this first cluster.
                for sector in sectors {
                    if total_bytes_read >= bytes_to_read {
                        break;
                    }
                    let buf_slice = &mut buf[total_bytes_read..];
                    let bytes_read = self.fs.read_sector(sector, 0, buf_slice).await?;
                    total_bytes_read += bytes_read;
                }
            }
        }

        'aligned_loop: for cluster_result in cluster_iter {
            if total_bytes_read >= bytes_to_read {
                break;
            }
            let cluster = cluster_result?;

            // Read all sectors in a full cluster.
            for sector in self.fs.cluster_to_sectors(cluster)? {
                if total_bytes_read >= bytes_to_read {
                    break 'aligned_loop;
                }
                let buf_slice = &mut buf[total_bytes_read..];
                let bytes_read = self.fs.read_sector(sector, 0, buf_slice).await?;
                total_bytes_read += bytes_read;
            }
        }

        // The bounds check on `effective_buf` throughout the loops ensure that
        // the final `read_sector` call will be passed a smaller slice,
        // correctly reading only the remaining bytes and handling the tail
        // misalignment automagically. See `self.fs.read_sector`.
        Ok(total_bytes_read)
    }
}
