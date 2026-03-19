use crate::kernel::cpu_id::CpuId;
use crate::memory::uaccess::copy_to_user;
use libkernel::memory::address::TUA;

pub async fn sys_getcpu(cpu_ptr: TUA<u32>, _node_ptr: TUA<u32>) -> libkernel::error::Result<usize> {
    let cpu_id = CpuId::this().value() as u32;
    copy_to_user(cpu_ptr, cpu_id).await?;
    // TODO: implement NUMA and write the node ID to node_ptr
    Ok(0)
}
