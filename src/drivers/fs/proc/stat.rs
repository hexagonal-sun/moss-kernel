use crate::arch::{Arch, ArchImpl};
use crate::drivers::timer::uptime;
use crate::kernel::cpu_id::CpuId;
use crate::process::clone::NUM_FORKS;
use crate::sched::{CpuStat, NUM_CONTEXT_SWITCHES, get_current_cpu_stat, get_other_cpu_stat};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use async_trait::async_trait;
use core::sync::atomic::Ordering;
use libkernel::CpuOps;
use libkernel::fs::attr::FileAttr;
use libkernel::fs::{InodeId, SimpleFile};

pub struct ProcStatInode {
    id: InodeId,
    attr: FileAttr,
}

impl ProcStatInode {
    pub fn new(inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: libkernel::fs::FileType::File,
                ..FileAttr::default()
            },
        }
    }
}

#[async_trait]
impl SimpleFile for ProcStatInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn getattr(&self) -> libkernel::error::Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn read(&self) -> libkernel::error::Result<Vec<u8>> {
        let mut stat_content = String::new();

        let mut cpu_stats = Vec::new();
        for cpu_id in 0..ArchImpl::cpu_count() {
            if cpu_id == ArchImpl::id() {
                cpu_stats.push(get_current_cpu_stat());
            } else {
                // TODO: Fill in real CPU stats.
                cpu_stats.push(get_other_cpu_stat(CpuId::from_value(cpu_id)));
            }
        }
        let total: CpuStat = cpu_stats
            .iter()
            .fold(CpuStat::default(), |acc, stat| CpuStat {
                user: acc.user + stat.user,
                nice: acc.nice + stat.nice,
                system: acc.system + stat.system,
                idle: acc.idle + stat.idle,
                iowait: acc.iowait + stat.iowait,
                irq: acc.irq + stat.irq,
                softirq: acc.softirq + stat.softirq,
                steal: acc.steal + stat.steal,
                guest: acc.guest + stat.guest,
                guest_nice: acc.guest_nice + stat.guest_nice,
            });
        cpu_stats.insert(0, total);
        for (i, stat) in cpu_stats.iter().enumerate() {
            let label = if i == 0 {
                "cpu".to_string()
            } else {
                format!("cpu{}", i - 1)
            };
            stat_content.push_str(&format!(
                "{label} {} {} {} {} {} {} {} {} {} {}\n",
                stat.user,
                stat.nice,
                stat.system,
                stat.idle,
                stat.iowait,
                stat.irq,
                stat.softirq,
                stat.steal,
                stat.guest,
                stat.guest_nice
            ));
        }
        stat_content.push_str(&format!(
            "ctxt {}\n",
            NUM_CONTEXT_SWITCHES.load(Ordering::Relaxed)
        ));
        stat_content.push_str(&format!("btime {}\n", uptime().as_secs()));
        stat_content.push_str(&format!(
            "processes {}\n",
            NUM_FORKS.load(Ordering::Relaxed)
        ));
        Ok(stat_content.into_bytes())
    }
}
