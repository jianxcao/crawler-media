use std::path::{Component, Path, PathBuf};

/// Resolve existing components through the filesystem and missing descendants
/// lexically. Never fall back to unchecked paths after a filesystem error.
pub(crate) fn resolved_path(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => result.push(prefix.as_os_str()),
            Component::RootDir => result.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            Component::Normal(name) => {
                result.push(name);
                match std::fs::symlink_metadata(&result) {
                    Ok(_) => result = result.canonicalize()?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(result)
}

/// `path` 是否落在 `root` 之下——**按两边解析后的路径比**。
///
/// 根在库里存的是操作员当初输入的拼写，调用方手里的路径可能是另一种拼写
/// （`/var` 与 `/private/var`、符号链接根）。直接 `starts_with` 会在这种别名上
/// 正确地说"不在"，所以凡是要在"拿到的路径"和"记录的根"之间做包含判断的地方，
/// 都必须走这里。解析失败（路径还不存在）时退回原始前缀比较，绝不因为解析失败
/// 就断言"不属于"。
pub fn path_under_root(path: &Path, root: &Path) -> bool {
    match (resolved_path(path), resolved_path(root)) {
        (Ok(resolved_path), Ok(resolved_root)) => resolved_path.starts_with(&resolved_root),
        _ => path.starts_with(root),
    }
}
