//! Supplemental disk-format tests of the MOSS driver (not host syscall tests).
use super::*;
use crate::{fs::BlockDevice, test::MockCpuOps};
use std::{fs, os::unix::fs::FileExt, process::Command};

struct ImageDevice(fs::File);

#[async_trait]
impl BlockDevice for ImageDevice {
    async fn read(&self, block: u64, bytes: &mut [u8]) -> Result<()> {
        self.0
            .read_exact_at(bytes, block * 512)
            .map_err(|_| KernelError::Other("image read"))
    }
    async fn write(&self, block: u64, bytes: &[u8]) -> Result<()> {
        self.0
            .write_all_at(bytes, block * 512)
            .map_err(|_| KernelError::Other("image write"))
    }
    fn block_size(&self) -> usize {
        512
    }
    async fn sync(&self) -> Result<()> {
        self.0
            .sync_all()
            .map_err(|_| KernelError::Other("image sync"))
    }
}

async fn mount(path: &std::path::Path) -> Arc<Ext4Filesystem<MockCpuOps>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    Ext4Filesystem::new(BlockBuffer::new(Box::new(ImageDevice(file))), 1)
        .await
        .unwrap()
}

fn run(command: &mut Command, allowed: &[i32]) {
    let output = command
        .output()
        .expect("install e2fsprogs to run this ignored test");
    assert!(
        allowed.contains(&output.status.code().unwrap_or(-1)),
        "{command:?}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[ignore = "requires mkfs.ext4 and e2fsck; creates isolated images under build/ext4-tests"]
async fn adapter_disk_formats() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../build/ext4-tests")
        .join(format!("{}-{stamp}", std::process::id()));
    fs::create_dir_all(&base).unwrap();
    // Populate an htree through the standard offline checker, then exercise
    // the MOSS driver's directory move on both indexed and linear directories.
    let seed = base.join("rootfs");
    fs::create_dir_all(seed.join("indexed")).unwrap();
    for index in 0..200 {
        fs::write(seed.join(format!("indexed/entry-{index:04}-padding")), []).unwrap();
    }

    for (kind, block_size, inode_size, features) in [
        ("ext2", "1024", "256", ""),
        ("ext4", "4096", "256", ""),
        ("ext4", "1024", "256", "metadata_csum_seed,^64bit"),
    ] {
        let image = base.join(format!("{kind}-{block_size}.img"));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&image)
            .unwrap();
        file.set_len(32 * 1024 * 1024).unwrap();
        drop(file);
        let mut mkfs = Command::new("mkfs.ext4");
        mkfs.args(["-q", "-F", "-t", kind, "-b", block_size, "-I", inode_size]);
        if !features.is_empty() {
            mkfs.args(["-O", features]);
        }
        run(mkfs.arg("-d").arg(&seed).arg(&image), &[0]);
        run(Command::new("e2fsck").args(["-fyD"]).arg(&image), &[0, 1]);

        let fs = mount(&image).await;
        let root = fs.root_inode().await.unwrap();
        let mode = FilePermissions::from_bits_truncate(0o755);
        let left = root
            .create("left", FileType::Directory, mode, None)
            .await
            .unwrap();
        let right = root
            .create("right", FileType::Directory, mode, None)
            .await
            .unwrap();
        let linear = left
            .create("linear", FileType::Directory, mode, None)
            .await
            .unwrap();
        let left_links = left.getattr().await.unwrap().nlinks;
        right
            .rename_from(left.clone(), "linear", "moved", false)
            .await
            .unwrap();
        assert_eq!(linear.lookup("..").await.unwrap().id(), right.id());
        assert_eq!(left.getattr().await.unwrap().nlinks, left_links - 1);
        let right_links = right.getattr().await.unwrap().nlinks;
        right
            .rename_from(right.clone(), "moved", "again", false)
            .await
            .unwrap();
        assert_eq!(right.getattr().await.unwrap().nlinks, right_links);
        right.unlink("again").await.unwrap();
        assert_eq!(right.getattr().await.unwrap().nlinks, right_links - 1);

        let indexed = root.lookup("indexed").await.unwrap();
        let ext_inode = indexed
            .as_any()
            .downcast_ref::<Ext4Inode<MockCpuOps>>()
            .unwrap();
        assert!(
            ext_inode
                .inner
                .lock()
                .await
                .flags()
                .contains(InodeFlags::DIRECTORY_HTREE)
        );
        right
            .rename_from(root.clone(), "indexed", "indexed", false)
            .await
            .unwrap();
        assert_eq!(indexed.lookup("..").await.unwrap().id(), right.id());
        assert!(right.unlink("indexed").await.is_err());
        for index in 0..200 {
            indexed
                .unlink(&format!("entry-{index:04}-padding"))
                .await
                .unwrap();
        }
        right.unlink("indexed").await.unwrap();

        for length in [1, 13, 59, 60, 61, 120] {
            let target = "t".repeat(length);
            root.symlink("sym", Path::new(&target)).await.unwrap();
            let link = root.lookup("sym").await.unwrap();
            assert_eq!(link.readlink().await.unwrap().as_str(), target);
            root.link("sym-hard", link.clone()).await.unwrap();
            root.unlink("sym").await.unwrap();
            assert_eq!(link.getattr().await.unwrap().nlinks, 1);
            assert_eq!(link.readlink().await.unwrap().as_str(), target);
            root.unlink("sym-hard").await.unwrap();
        }

        let file = root
            .create("file", FileType::File, mode, None)
            .await
            .unwrap();
        root.link("hard", file.clone()).await.unwrap();
        let hard = root.lookup("hard").await.unwrap();
        let data = vec![0x5a; 16385];
        for size in [5, 1023, 1024, 4095, 4096, 4097, 0] {
            file.truncate(0).await.unwrap();
            let mut written = 0;
            while written < data.len() {
                let count = file
                    .write_at(written as u64, &data[written..])
                    .await
                    .unwrap();
                assert!(count > 0);
                written += count;
            }
            hard.truncate(size).await.unwrap();
            assert_eq!(file.getattr().await.unwrap().size, size);
            file.truncate(data.len() as u64).await.unwrap();
            let mut bytes = vec![0xff; data.len() - size as usize];
            assert_eq!(hard.read_at(size, &mut bytes).await.unwrap(), bytes.len());
            assert!(bytes.iter().all(|&byte| byte == 0));
        }
        // A shrink inside a hole must not allocate a new block to clear it.
        file.truncate(0).await.unwrap();
        file.truncate(16385).await.unwrap();
        let before = file
            .as_any()
            .downcast_ref::<Ext4Inode<MockCpuOps>>()
            .unwrap()
            .inner
            .lock()
            .await
            .blocks();
        file.truncate(4097).await.unwrap();
        assert_eq!(
            file.as_any()
                .downcast_ref::<Ext4Inode<MockCpuOps>>()
                .unwrap()
                .inner
                .lock()
                .await
                .blocks(),
            before
        );
        root.unlink("file").await.unwrap();
        root.unlink("hard").await.unwrap();
        root.unlink("left").await.unwrap();
        root.unlink("right").await.unwrap();
        fs.sync().await.unwrap();
        drop(fs);
        run(Command::new("e2fsck").args(["-fn"]).arg(&image), &[0]);
        let reopened = mount(&image).await;
        let root = reopened.root_inode().await.unwrap();
        assert!(root.lookup("right").await.is_err());
        std::println!(
            "verified {kind}, block={block_size}, inode={inode_size}, features={features}"
        );
    }
}
