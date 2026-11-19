//! 内部只读工作区浏览；所有子路径访问均相对于保留的目录句柄。
/*
Read-only path guard adapted from Code-Codex crates/workspace-service/src/path_guard.rs.

MIT License

Copyright (c) 2026 Code-Codex contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/

use std::collections::hash_map::RandomState;
use std::fs::File;
use std::hash::BuildHasher;
use std::sync::OnceLock;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt as _;
use cap_fs_ext::{OpenOptionsFollowExt as _, OpenOptionsSyncExt as _};
use cap_primitives::ambient_authority;
use cap_primitives::fs::{
    FollowSymlinks, Metadata as CapabilityMetadata, OpenOptions, open, open_ambient,
    open_ambient_dir, read_base_dir, stat,
};
#[cfg(windows)]
use cap_primitives::fs::MetadataExt as _;
use same_file::Handle;
use serde::Deserialize;
use serde_json::{Value, json};

const MAX_ENTRIES: usize = 10_000;
const PREVIEW_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("invalid_path: 路径无效")]
    InvalidPath,
    #[error("invalid_request: 请求参数无效")]
    InvalidRequest,
    #[error("session_unavailable: 无法读取会话")]
    SessionUnavailable,
    #[error("outside_workspace: 工作区身份已变化或路径包含链接，请刷新工作区")]
    OutsideWorkspace,
    #[error("not_directory: 路径不是目录")]
    NotDirectory,
    #[error("not_regular_file: 仅支持普通文件")]
    NotRegularFile,
    #[error("not_found: 路径不存在")]
    NotFound,
    #[error("access_denied: 无权读取路径")]
    AccessDenied,
    #[error("too_many_entries: 目录达到枚举安全上限")]
    TooManyEntries,
    #[error("invalid_cursor: 分页位置超出目录范围")]
    InvalidCursor,
    #[error("binary_file: 不支持二进制预览")]
    BinaryFile,
    #[error("invalid_utf8: 文件不是有效 UTF-8")]
    InvalidUtf8,
    #[error("io_error: 读取失败")]
    Io,
}

impl WorkspaceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPath => "invalid_path",
            Self::InvalidRequest => "invalid_request",
            Self::SessionUnavailable => "session_unavailable",
            Self::OutsideWorkspace => "outside_workspace",
            Self::NotDirectory => "not_directory",
            Self::NotRegularFile => "not_regular_file",
            Self::NotFound => "not_found",
            Self::AccessDenied => "access_denied",
            Self::TooManyEntries => "too_many_entries",
            Self::InvalidCursor => "invalid_cursor",
            Self::BinaryFile => "binary_file",
            Self::InvalidUtf8 => "invalid_utf8",
            Self::Io => "io_error",
        }
    }
}

type Result<T> = std::result::Result<T, WorkspaceError>;

fn map_io(error: std::io::Error) -> WorkspaceError {
    match error.kind() {
        std::io::ErrorKind::NotFound => WorkspaceError::NotFound,
        std::io::ErrorKind::PermissionDenied => WorkspaceError::AccessDenied,
        std::io::ErrorKind::NotADirectory => WorkspaceError::NotDirectory,
        _ => WorkspaceError::Io,
    }
}

fn map_capability_open(error: std::io::Error) -> WorkspaceError {
    match error.kind() {
        std::io::ErrorKind::NotFound => WorkspaceError::NotFound,
        std::io::ErrorKind::PermissionDenied => WorkspaceError::AccessDenied,
        std::io::ErrorKind::NotADirectory => WorkspaceError::NotDirectory,
        _ => WorkspaceError::OutsideWorkspace,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    session_id: String,
    path: String,
    cursor: Option<usize>,
    limit: Option<usize>,
    root_token: Option<String>,
}

pub fn route(payload: &Value, preview: bool) -> Result<Value> {
    route_with_home(&crate::relay_config::default_codex_home_dir(), payload, preview)
}

/// 显式 home 供隔离测试复用；桥接请求不能传入 home 或 root。
pub fn route_with_home(home: &Path, payload: &Value, preview: bool) -> Result<Value> {
    let request: Request = serde_json::from_value(payload.clone())
        .map_err(|_| WorkspaceError::InvalidRequest)?;
    if request.session_id.trim().is_empty() {
        return Err(WorkspaceError::InvalidRequest);
    }
    let initial_list = !preview && request.path.is_empty() && request.cursor.unwrap_or(0) == 0;
    if request.root_token.is_none() && !initial_list {
        return Err(WorkspaceError::InvalidRequest);
    }
    let session = crate::extension_history::get_session(home, &request.session_id)
        .map_err(|_| WorkspaceError::SessionUnavailable)?;
    let cwd = session.get("cwd").and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty()).ok_or(WorkspaceError::InvalidPath)?;
    let explorer = WorkspaceExplorer::open(Path::new(cwd))?;
    if let Some(expected) = request.root_token.as_deref() {
        if expected != explorer.root_token()? {
            return Err(WorkspaceError::OutsideWorkspace);
        }
    }
    if preview {
        explorer.preview(&request.path)
    } else {
        explorer.list(&request.path, request.cursor, request.limit)
    }
}

pub struct WorkspaceExplorer {
    canonical: PathBuf,
    root: File,
}

impl WorkspaceExplorer {
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_with(root, || {})
    }

    fn open_with(root: &Path, after_binding: impl FnOnce()) -> Result<Self> {
        if !root.is_absolute() {
            return Err(WorkspaceError::InvalidPath);
        }
        // 先绑定权限再解析名称，防止 canonicalize 与打开之间替换根。
        let bound = open_ambient_dir(root, ambient_authority()).map_err(map_io)?;
        check_directory(&bound)?;
        after_binding();
        let canonical = dunce::canonicalize(root).map_err(map_io)?;
        let verified = open_ambient_directory_nofollow(&canonical)?;
        if !same_identity(&bound, &verified)? {
            return Err(WorkspaceError::OutsideWorkspace);
        }
        let explorer = Self { canonical, root: verified };
        explorer.verify_root()?;
        Ok(explorer)
    }

    fn root_token(&self) -> Result<String> {
        // Hash 使用实际文件身份，不使用路径；随机种子在当前进程内保持不变。
        static STATE: OnceLock<RandomState> = OnceLock::new();
        let identity = Handle::from_file(self.root.try_clone().map_err(map_io)?).map_err(map_io)?;
        Ok(format!("{:016x}", STATE.get_or_init(RandomState::new).hash_one(identity)))
    }

    fn verify_root(&self) -> Result<()> {
        let current = open_ambient_directory_nofollow(&self.canonical)
            .map_err(|_| WorkspaceError::OutsideWorkspace)?;
        if !same_identity(&self.root, &current)? {
            return Err(WorkspaceError::OutsideWorkspace);
        }
        Ok(())
    }

    pub fn list(&self, relative: &str, cursor: Option<usize>, limit: Option<usize>) -> Result<Value> {
        let clean = validate_relative(relative)?;
        let limit = limit.unwrap_or(200).min(500);
        if limit == 0 { return Err(WorkspaceError::InvalidRequest); }
        self.verify_root()?;
        let directory = DirectoryCapability::open(&self.root, &clean)?;
        let handle = directory.handle();
        let mut items = Vec::new();
        // 达到上限即拒绝，不返回取决于系统枚举顺序的随机子集。
        for (index, entry) in read_base_dir(handle).map_err(map_io)?.take(MAX_ENTRIES).enumerate() {
            if index + 1 == MAX_ENTRIES { return Err(WorkspaceError::TooManyEntries); }
            let entry = entry.map_err(map_io)?;
            let name = entry.file_name().into_string().map_err(|_| WorkspaceError::InvalidPath)?;
            let child = if relative.is_empty() { name.clone() } else {
                format!("{}/{}", clean.to_string_lossy().replace('\\', "/"), name)
            };
            // 不允许系统文件名经分隔符规范化后成为另一个路径。
            if name.contains(['/', '\\']) { return Err(WorkspaceError::InvalidPath); }
            validate_relative(&child)?;
            let metadata = match stat(handle, Path::new(&name), FollowSymlinks::No) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(map_io(error)),
            };
            let kind = if is_capability_link_or_reparse(&metadata) { "link" }
                else if metadata.is_dir() { "directory" }
                else if metadata.is_file() { "file" } else { "other" };
            items.push((kind != "directory", name, child, kind));
        }
        items.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let cursor = cursor.unwrap_or(0);
        if cursor > items.len() { return Err(WorkspaceError::InvalidCursor); }
        let end = cursor.saturating_add(limit).min(items.len());
        let next = (end < items.len()).then_some(end);
        let page: Vec<_> = items[cursor..end].iter().map(|(_, name, path, kind)| {
            json!({"name": name, "path": path, "kind": kind})
        }).collect();
        self.verify_root()?;
        Ok(json!({"items": page, "nextCursor": next, "truncated": false, "rootToken": self.root_token()?}))
    }

    pub fn preview(&self, relative: &str) -> Result<Value> {
        let clean = validate_relative(relative)?;
        let name = clean.file_name().ok_or(WorkspaceError::NotRegularFile)?;
        self.verify_root()?;
        let directory = DirectoryCapability::open(&self.root, clean.parent().unwrap_or(Path::new("")))?;
        let file = open_regular_file_nofollow(directory.handle(), Path::new(name))?;
        let mut bytes = Vec::new();
        file.take((PREVIEW_BYTES + 1) as u64).read_to_end(&mut bytes).map_err(map_io)?;
        let truncated = bytes.len() > PREVIEW_BYTES;
        if bytes.iter().any(|byte| *byte < 0x20 && !matches!(*byte, b'\t' | b'\r' | b'\n') || *byte == 0x7f) {
            return Err(WorkspaceError::BinaryFile);
        }
        // 先验证探测字节；仅因读取上限造成的不完整尾字符可以丢弃。
        let valid_end = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            Err(error) if truncated && error.error_len().is_none() => error.valid_up_to(),
            Err(_) => return Err(WorkspaceError::InvalidUtf8),
        };
        let text = std::str::from_utf8(&bytes[..valid_end]).map_err(|_| WorkspaceError::InvalidUtf8)?;
        let mut end = text.len().min(PREVIEW_BYTES);
        while !text.is_char_boundary(end) { end -= 1; }
        self.verify_root()?;
        Ok(json!({"path": clean.to_string_lossy().replace('\\', "/"), "text": &text[..end], "truncated": truncated}))
    }
}

fn directory_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(windows)]
    {
        use cap_primitives::fs::OpenOptionsExt as _;
        // 与源实现一致：允许删除共享；不用会移除 FILE_SHARE_DELETE 的 maybe_dir。
        options.share_mode(0x1 | 0x2 | 0x4).custom_flags(0x0200_0000);
    }
    #[cfg(not(windows))]
    options.maybe_dir(true);
    options
}

fn check_directory(file: &File) -> Result<()> {
    let metadata = file.metadata().map_err(map_io)?;
    if is_link_or_reparse(&metadata) { return Err(WorkspaceError::OutsideWorkspace); }
    if !metadata.is_dir() { return Err(WorkspaceError::NotDirectory); }
    Ok(())
}

fn open_ambient_directory_nofollow(path: &Path) -> Result<File> {
    let file = open_ambient(path, &directory_options(), ambient_authority()).map_err(map_capability_open)?;
    check_directory(&file)?;
    Ok(file)
}

fn same_identity(left: &File, right: &File) -> Result<bool> {
    let left = Handle::from_file(left.try_clone().map_err(map_io)?).map_err(map_io)?;
    let right = Handle::from_file(right.try_clone().map_err(map_io)?).map_err(map_io)?;
    Ok(left == right)
}

struct DirectoryCapability {
    handles: Vec<File>,
}

impl DirectoryCapability {
    fn open(root: &File, relative: &Path) -> Result<Self> {
        let mut handles = vec![root.try_clone().map_err(map_io)?];
        for component in relative.components() {
            let Component::Normal(part) = component else { return Err(WorkspaceError::InvalidPath); };
            let opened = open(handles.last().expect("root handle"), Path::new(part), &directory_options())
                .map_err(map_capability_open)?;
            check_directory(&opened)?;
            handles.push(opened);
        }
        Ok(Self { handles })
    }

    fn handle(&self) -> &File {
        self.handles.last().expect("root handle")
    }
}

fn open_regular_file_nofollow(parent: &File, path: &Path) -> Result<File> {
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(WorkspaceError::InvalidPath);
    }
    let candidate = stat(parent, path, FollowSymlinks::No).map_err(map_capability_open)?;
    if is_capability_link_or_reparse(&candidate) { return Err(WorkspaceError::OutsideWorkspace); }
    if !candidate.is_file() { return Err(WorkspaceError::NotRegularFile); }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    #[cfg(windows)]
    {
        use cap_primitives::fs::OpenOptionsExt as _;
        options.share_mode(0x1 | 0x2 | 0x4);
    }
    let file = open(parent, path, &options).map_err(map_capability_open)?;
    let metadata = file.metadata().map_err(map_io)?;
    if is_link_or_reparse(&metadata) { return Err(WorkspaceError::OutsideWorkspace); }
    if !metadata.is_file() { return Err(WorkspaceError::NotRegularFile); }
    Ok(file)
}

fn is_capability_link_or_reparse(metadata: &CapabilityMetadata) -> bool {
    if metadata.file_type().is_symlink() { return true; }
    #[cfg(windows)]
    { metadata.file_attributes() & 0x400 != 0 }
    #[cfg(not(windows))]
    { false }
}

fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() { return true; }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    { false }
}

fn validate_relative(relative: &str) -> Result<PathBuf> {
    if relative.len() > 1024 || relative.contains(['\0', ':'])
        || relative.starts_with(['/', '\\'])
        || (relative.contains('/') && relative.contains('\\')) {
        return Err(WorkspaceError::InvalidPath);
    }
    if relative.is_empty() { return Ok(PathBuf::new()); }
    let normalized = relative.replace('\\', "/");
    let parts: Vec<_> = normalized.split('/').collect();
    if parts.len() > 128 { return Err(WorkspaceError::InvalidPath); }
    for part in parts {
        if part.is_empty() || part == "." || part == ".." || part.ends_with([' ', '.'])
            || part.chars().any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*' | '\0'..='\u{1f}')) {
            return Err(WorkspaceError::InvalidPath);
        }
        let upper = part.split('.').next().unwrap_or_default().to_ascii_uppercase();
        let numbered = ["COM", "LPT"].iter().any(|prefix| upper.strip_prefix(*prefix).is_some_and(|suffix| {
            matches!(suffix, "¹" | "²" | "³") || suffix.parse::<u8>().ok().is_some_and(|n| (1..=9).contains(&n))
        }));
        if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") || numbered {
            return Err(WorkspaceError::InvalidPath);
        }
    }
    let path = PathBuf::from(normalized);
    if path.components().any(|c| !matches!(c, Component::Normal(_))) { return Err(WorkspaceError::InvalidPath); }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_root_replacement_between_binding_and_canonicalization() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let result = WorkspaceExplorer::open_with(&root, || {
            std::fs::rename(&root, parent.path().join("original")).unwrap();
            std::fs::create_dir(&root).unwrap();
        });
        assert!(matches!(result, Err(WorkspaceError::OutsideWorkspace)));
    }
}
