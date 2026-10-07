use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{LibraryError, TransferMode};

pub fn transfer_file(src: &Path, dest: &Path, mode: TransferMode) -> Result<(), LibraryError> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if dest.is_dir() {
        return Err(LibraryError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "destination path is an existing directory: {}",
                dest.display()
            ),
        )));
    }
    // 源文件与目标文件若是同一个规范化路径（或指向同一硬链接/符号链接实体），直接作为 no-op 成功返回
    if src == dest {
        return Ok(());
    }
    if let (Ok(src_canon), Ok(dest_canon)) = (src.canonicalize(), dest.canonicalize()) {
        if src_canon == dest_canon {
            return Ok(());
        }
    }
    let temp = temporary_path(dest);
    if temp.exists() {
        let _ = fs::remove_file(&temp);
    }

    // 第一阶段：生成暂存文件。Move 模式在暂存阶段先复制，保留原文件 src
    let stage_res = match mode {
        TransferMode::Copy | TransferMode::Move => fs::copy(src, &temp).map(|_| ()),
        TransferMode::Hardlink => {
            if fs::hard_link(src, &temp).is_err() {
                tracing::warn!(src = %src.display(), dest = %dest.display(), "硬链接失败（可能跨设备），回退为复制");
                fs::copy(src, &temp).map(|_| ())
            } else {
                Ok(())
            }
        }
    };
    if let Err(e) = stage_res {
        if temp.exists() {
            let _ = fs::remove_file(&temp);
        }
        return Err(LibraryError::Io(e));
    }

    // 第二阶段：发布暂存文件到目标 dest
    let publish_res = if dest.exists() {
        fs::remove_file(dest).and_then(|_| fs::rename(&temp, dest))
    } else {
        fs::rename(&temp, dest)
    };

    match publish_res {
        Ok(()) => {
            // 只有当目标彻底成功发布后，Move 模式才安全移除源文件
            if mode == TransferMode::Move {
                if let Err(e) = fs::remove_file(src) {
                    tracing::warn!(
                        src = %src.display(),
                        dest = %dest.display(),
                        error = %e,
                        "文件已成功发布到目标，但清理源文件失败"
                    );
                }
            }
            Ok(())
        }
        Err(e) => {
            if temp.exists() {
                let _ = fs::remove_file(&temp);
            }
            Err(LibraryError::Io(e))
        }
    }
}

fn temporary_path(dest: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = dest
        .file_name()
        .unwrap_or_else(|| "media".as_ref())
        .to_os_string();
    name.push(format!(".transfer-{}-{nonce}", std::process::id()));
    dest.with_file_name(name)
}
