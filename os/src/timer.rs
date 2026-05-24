use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref TIMER_MANAGER: Mutex<TimerManager> = Mutex::new(TimerManager::new());
}

pub struct TimerManager {
    // 正向索引：到期时间(ms) -> 挂在该时间点的进程 PID 列表
    events: BTreeMap<usize, Vec<usize>>,
    // 反向索引：进程 PID -> 它当前的闹钟到期时间(ms)
    pid_map: BTreeMap<usize, usize>,
}

impl TimerManager {
    pub fn new() -> Self {
        Self {
            events: BTreeMap::new(),
            pid_map: BTreeMap::new(),
        }
    }

    /// 取消闹钟的核心逻辑，O(log N) 复杂度
    pub fn cancel_alarm(&mut self, pid: usize) -> usize {
        // 1. O(log N) 极速找到进程对应的闹钟时间
        if let Some(expire_ms) = self.pid_map.remove(&pid) {
            // 2. 去事件树里把它删掉
            if let Some(pids) = self.events.get_mut(&expire_ms) {
                // retain 保留不等于该 pid 的元素
                pids.retain(|&x| x != pid); 
                // 如果这个时间点没有其他闹钟了，把空节点也干掉，防止内存泄露
                if pids.is_empty() {
                    self.events.remove(&expire_ms);
                }
            }
            return expire_ms;
        }
        0 // 没有旧闹钟
    }

    /// 设置闹钟
    pub fn set_alarm(&mut self, pid: usize, current_ms: usize, delay_ms: usize) -> usize {
        // 先无脑注销旧闹钟
        let old_expire_ms = self.cancel_alarm(pid);

        // 如果传参不是 0，说明要设新闹钟
        if delay_ms > 0 {
            let new_expire = current_ms + delay_ms;
            self.events.entry(new_expire).or_default().push(pid);
            self.pid_map.insert(pid, new_expire);
        }

        // 返回旧闹钟剩余的秒数
        if old_expire_ms > current_ms {
            old_expire_ms - current_ms
        } else {
            0
        }
    }

    /// 时钟中断调用：只收集数据，绝不碰 PCB！
    pub fn tick(&mut self, current_ms: usize) -> Vec<usize> {
        let mut expired_pids = Vec::new();
        
        while let Some((&expire_ms, _)) = self.events.first_key_value() {
            if expire_ms <= current_ms {
                // 弹出整批到期的 PID
                let pids = self.events.pop_first().unwrap().1;
                for pid in &pids {
                    // 同步清理反向索引
                    self.pid_map.remove(pid);
                }
                expired_pids.extend(pids);
            } else {
                break;
            }
        }
        
        // 返回出去让外层慢慢发信号，彻底解耦全局锁！
        expired_pids
    }
}