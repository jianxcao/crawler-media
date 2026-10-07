use std::fs::{self, File};
use std::io::BufReader;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use flate2::read::GzDecoder;
use parking_lot::Mutex;
use tar::Archive;

pub const DEFAULT_OBSCURA_PORT: u16 = 9223;
pub const OBSCURA_VERSION: &str = "v0.2.3";

pub struct ObscuraManager {
    bin_dir: PathBuf,
    port: u16,
    child: Arc<Mutex<Option<Child>>>,
    is_downloading: Arc<AtomicBool>,
}

impl ObscuraManager {
    pub fn new(data_dir: &Path) -> Self {
        let bin_dir = data_dir.join("bin");
        Self {
            bin_dir,
            port: DEFAULT_OBSCURA_PORT,
            child: Arc::new(Mutex::new(None)),
            is_downloading: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn binary_path(&self) -> PathBuf {
        if cfg!(windows) {
            self.bin_dir.join("obscura.exe")
        } else {
            self.bin_dir.join("obscura")
        }
    }

    pub fn is_installed(&self) -> bool {
        self.binary_path().is_file()
    }

    pub fn is_downloading(&self) -> bool {
        self.is_downloading.load(Ordering::SeqCst)
    }

    pub fn is_running(&self) -> bool {
        // 先检查端口是否存活且能建立 TCP 连接
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", self.port).parse().unwrap(),
            Duration::from_millis(200),
        )
        .is_ok()
    }

    pub fn cdp_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// 下载并安装 Obscura 对应平台二进制
    pub fn ensure_installed(&self) -> Result<(), String> {
        if self.is_installed() {
            return Ok(());
        }

        if self
            .is_downloading
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("Obscura 正在下载中，请稍候...".into());
        }

        let res = self.download_and_extract();
        self.is_downloading.store(false, Ordering::SeqCst);
        res
    }

    fn download_and_extract(&self) -> Result<(), String> {
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        let asset_name = match (os, arch) {
            ("linux", "x86_64") => "obscura-x86_64-linux.tar.gz",
            ("linux", "aarch64") => "obscura-aarch64-linux.tar.gz",
            ("macos", "x86_64") => "obscura-x86_64-macos.tar.gz",
            ("macos", "aarch64") => "obscura-aarch64-macos.tar.gz",
            _ => {
                return Err(format!(
                    "暂不支持的操作系统或架构: os={}, arch={}",
                    os, arch
                ));
            }
        };

        let download_url = format!(
            "https://github.com/h4ckf0r0day/obscura/releases/download/{}/{}",
            OBSCURA_VERSION, asset_name
        );

        tracing::info!(
            url = %download_url,
            "【Obscura】开始自动下载轻量级防检测无头浏览器引擎"
        );

        fs::create_dir_all(&self.bin_dir)
            .map_err(|e| format!("创建目录失败 {}: {e}", self.bin_dir.display()))?;

        let tar_gz_path = self.bin_dir.join(asset_name);

        // 下载归档文件
        let mut response = ureq::get(&download_url)
            .call()
            .map_err(|e| format!("下载 Obscura 失败: {e}"))?;

        let mut out_file = File::create(&tar_gz_path)
            .map_err(|e| format!("创建文件失败 {}: {e}", tar_gz_path.display()))?;

        std::io::copy(&mut response.body_mut().as_reader(), &mut out_file)
            .map_err(|e| format!("写入文件失败: {e}"))?;

        tracing::info!("【Obscura】下载完成，正在解压...");

        // 解压 tar.gz
        let tar_gz_file = File::open(&tar_gz_path).map_err(|e| format!("打开下载归档失败: {e}"))?;
        let tar = GzDecoder::new(BufReader::new(tar_gz_file));
        let mut archive = Archive::new(tar);

        archive
            .unpack(&self.bin_dir)
            .map_err(|e| format!("解压 Obscura 失败: {e}"))?;

        // 清理安装包
        let _ = fs::remove_file(&tar_gz_path);

        // 设置可执行权限 (Unix)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let bin_path = self.binary_path();
            if bin_path.exists() {
                let _ = fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755));
            }
        }

        if !self.is_installed() {
            return Err("解压后未找到 obscura 二进制程序".into());
        }

        tracing::info!(
            path = %self.binary_path().display(),
            "【Obscura】浏览器引擎安装成功"
        );
        Ok(())
    }

    /// 启动 Obscura 守护进程
    pub fn start(&self) -> Result<(), String> {
        if self.is_running() {
            tracing::info!("【Obscura】服务已经在运行中，端口: {}", self.port);
            return Ok(());
        }

        self.ensure_installed()?;

        let bin_path = self.binary_path();
        tracing::info!(
            path = %bin_path.display(),
            port = self.port,
            "【Obscura】正在启动防检测浏览器守护进程..."
        );

        let child = Command::new(&bin_path)
            .arg("serve")
            .arg("--port")
            .arg(self.port.to_string())
            .spawn()
            .map_err(|e| format!("启动 Obscura 失败: {e}"))?;

        *self.child.lock() = Some(child);

        // 等待服务监听端口就绪 (最多等待 5 秒)
        for _ in 0..25 {
            std::thread::sleep(Duration::from_millis(200));
            if self.is_running() {
                tracing::info!("【Obscura】防检测浏览器引擎启动就绪，端口: {}", self.port);
                return Ok(());
            }
        }

        Err("Obscura 进程已拉起但端口响应超时".into())
    }

    /// 停止 Obscura 守护进程
    pub fn stop(&self) {
        let mut guard = self.child.lock();
        if let Some(mut child) = guard.take() {
            tracing::info!("【Obscura】正在停止守护进程...");
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
