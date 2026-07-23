//! IPC 命名空间
//! nsproxy 暂时放在此处

use super::msg::*;
use super::shm::*;

use alloc::sync::Arc;
use alloc::task;
use spin::mutex::Mutex;

/// 命名空间代理（NameSpace Proxy）
///
/// 每个进程通过 NsProxy 间接访问所有 namespace
/// fork 引用同一个 NsProxy（Arc）
/// unshare 时新建内部结构并替换
pub struct NsProxy {
    inner: Arc<Mutex<NsProxyInner>>,
}

impl NsProxy {
    /// 用ipc_ns新建
    pub fn new(ipc_ns: IPCNamespace) -> Self {
        Self {
            inner: Arc::new(Mutex::new(NsProxyInner::new(ipc_ns))),
        }
    }

    /// 单开新的 namespace
    /// 新建 ns 并以此新建 inner，替换当前 inner
    pub fn unshare(&mut self) {
        self.inner = Self::new_inner();
    }

    /// 新建ns并以此新建proxy inner，返回新inner
    pub fn new_inner() -> Arc<Mutex<NsProxyInner>> {
        Arc::new(Mutex::new(NsProxyInner::new(IPCNamespace::new())))
    }

    /// 获取ipc_ns的Arc（克隆）
    pub fn ipc_namespace(&self) -> Arc<Mutex<IPCNamespace>> {
        self.inner.lock().ipc_ns.clone()
    }
}

impl Clone for NsProxy {
    // inner实际上只克隆arc包装
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// nsproxy 内部状态
/// 
/// linux中这里还有其他命名空间
/// 当前实现只用于ipc_ns，待完善
pub struct NsProxyInner {
    ipc_ns: Arc<Mutex<IPCNamespace>>,
    // TODO：其他ns
}

impl NsProxyInner {
    pub fn new(ipc_ns: IPCNamespace) -> Self {
        Self { 
            ipc_ns: Arc::new(Mutex::new(ipc_ns))
        }
    }
}

/// ipc命名空间
pub struct IPCNamespace {
    msg_man: MsgManager,
    shm_man: ShmManager,
    // todo：可以加信号量等
}

impl IPCNamespace {
    pub fn new() -> Self {
        Self {
            msg_man: MsgManager::new(),
            shm_man: ShmManager::new(),
        }
    }
    pub fn msg_manager(&mut self) -> &mut MsgManager {
        &mut self.msg_man
    }
    pub fn shm_manager(&mut self) -> &mut ShmManager {
        &mut self.shm_man
    }
}

impl Default for IPCNamespace {
    fn default() -> Self {
        Self::new()
    }
}

use crate::process::current_task;
pub fn current_ipc_namespace() -> Arc<Mutex<IPCNamespace>> {
    let task = current_task().unwrap();
    let task_inner = task.inner_exclusive_access();
    let ns_proxy = &task_inner.nsproxy;
    let ipc_ns = ns_proxy.ipc_namespace();
    ipc_ns
}