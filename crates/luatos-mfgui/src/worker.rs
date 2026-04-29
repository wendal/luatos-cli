// 后台刷机线程 — 通过 mpsc channel 向 UI 发送进度消息。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use luatos_flash::{FlashProgress, ProgressCallback};

/// 工作线程向 UI 发送的消息类型。
pub enum WorkerMsg {
    /// 刷机进度更新
    Progress(FlashProgress),
    /// 任务结束（Ok=成功, Err=失败/取消信息）
    Done(Result<(), String>),
}

/// 后台刷机工作线程句柄。
pub struct FlashWorker {
    /// 取消标志（设为 true 时刷机线程将尽快退出）
    pub cancel: Arc<AtomicBool>,
    /// 接收工作线程消息的通道
    rx: Receiver<WorkerMsg>,
    /// 后台线程句柄（Option 便于 drop 时 join）
    _handle: JoinHandle<()>,
}

impl FlashWorker {
    /// 启动后台刷机线程并返回 FlashWorker 句柄。
    ///
    /// `soc_path` — SOC 固件文件路径  
    /// `port`     — UART 串口名称；EC718 系列传 None，自动检测 USB 设备
    pub fn spawn(soc_path: String, port: Option<String>) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let cancel_clone = Arc::clone(&cancel);

        let handle = std::thread::spawn(move || {
            run_flash(soc_path, port, tx, cancel_clone);
        });

        Self { cancel, rx, _handle: handle }
    }

    /// 请求取消当前刷机操作（仅设置标志，不阻塞等待）。
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// 非阻塞轮询，返回下一条消息（若无消息则返回 None）。
    pub fn poll(&self) -> Option<WorkerMsg> {
        self.rx.try_recv().ok()
    }
}

// ─── 内部实现 ──────────────────────────────────────────────────────────────────

/// 后台线程主函数：执行刷机并将结果通过 channel 发送给 UI。
fn run_flash(soc_path: String, port: Option<String>, tx: Sender<WorkerMsg>, cancel: Arc<AtomicBool>) {
    let result = do_flash(&soc_path, port.as_deref(), &tx, Arc::clone(&cancel));
    let outcome = result.map_err(|e| e.to_string());
    // Done 消息在 Progress.done 之后发送，作为兜底保证
    let _ = tx.send(WorkerMsg::Done(outcome));
}

/// 根据芯片类型分发到对应刷机函数。
fn do_flash(soc_path: &str, port: Option<&str>, tx: &Sender<WorkerMsg>, cancel: Arc<AtomicBool>) -> anyhow::Result<()> {
    let info = luatos_soc::read_soc_info(soc_path)?;
    let chip = info.chip.chip_type.as_str();

    match chip {
        "bk72xx" | "air8101" => {
            let port_str = port.ok_or_else(|| anyhow::anyhow!("bk72xx/air8101 需要指定串口"))?;
            let cb = make_cb(tx);
            luatos_flash::bk7258::flash_bk7258(soc_path, None, port_str, None, Arc::clone(&cancel), cb)?;
        }
        "air6208" | "air101" | "air103" | "air601" => {
            let port_str = port.ok_or_else(|| anyhow::anyhow!("XT804 系列需要指定串口"))?;
            let cb = make_cb(tx);
            luatos_flash::xt804::flash_xt804(soc_path, port_str, cb, Arc::clone(&cancel))?;
        }
        "air1601" | "ccm4211" => {
            let port_str = port.ok_or_else(|| anyhow::anyhow!("CCM4211/Air1601 需要指定串口"))?;
            let cb = make_cb(tx);
            luatos_flash::ccm4211::flash_ccm4211(soc_path, port_str, &cb, Arc::clone(&cancel))?;
        }
        "ec7xx" | "air8000" | "air780epm" | "air780ehm" | "air780ehv" | "air780ehg" | "air8000m" => {
            // EC718 系列：先进入下载模式（自动检测 USB），再刷机
            let cb = make_cb(tx);
            let boot_port = luatos_flash::ec718::auto_enter_boot_mode(port, &cb)?;
            let cb2 = make_cb(tx);
            luatos_flash::ec718::flash_ec718(soc_path, &boot_port, &cb2, Arc::clone(&cancel))?;
        }
        "sf32lb58" => {
            let port_str = port.ok_or_else(|| anyhow::anyhow!("SF32LB58 需要指定串口"))?;
            let cb = make_cb(tx);
            luatos_flash::sf32lb5x::flash_sf32lb5x(soc_path, port_str, None, cb, Arc::clone(&cancel), None, None)?;
        }
        other => anyhow::bail!("不支持的芯片类型: {other}"),
    }

    Ok(())
}

/// 创建将进度事件转发到 channel 的 ProgressCallback。
fn make_cb(tx: &Sender<WorkerMsg>) -> ProgressCallback {
    let tx = tx.clone();
    Box::new(move |p: &FlashProgress| {
        let _ = tx.send(WorkerMsg::Progress(p.clone()));
    })
}

// ─── 单元测试 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn worker_cancel_sets_flag() {
        let (tx, _rx) = mpsc::channel::<WorkerMsg>();
        let cancel = Arc::new(AtomicBool::new(false));
        // 模拟 cancel 行为（不启动实际刷机线程）
        assert!(!cancel.load(Ordering::Relaxed));
        cancel.store(true, Ordering::Relaxed);
        assert!(cancel.load(Ordering::Relaxed));
        drop(tx);
    }

    #[test]
    fn worker_poll_empty_returns_none() {
        let (tx, rx) = mpsc::channel::<WorkerMsg>();
        // 不发送任何消息，poll 应返回 None
        let result = rx.try_recv();
        assert!(result.is_err()); // TryRecvError::Empty
        drop(tx);
    }

    #[test]
    fn worker_msg_progress_forwarded() {
        let (tx, rx) = mpsc::channel::<WorkerMsg>();
        let cb = make_cb(&tx);
        let p = FlashProgress::info("test", 50.0, "half done");
        cb(&p);

        let msg = rx.try_recv().expect("应收到消息");
        match msg {
            WorkerMsg::Progress(fp) => {
                assert_eq!(fp.stage, "test");
                assert_eq!(fp.percent, 50.0);
                assert_eq!(fp.message, "half done");
            }
            WorkerMsg::Done(_) => panic!("期望 Progress 消息"),
        }
        drop(tx);
    }
}
