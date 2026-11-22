//! 检查更新：手动触发的版本查询、镜像下载与单文件替换落地。
//!
//! 设计见 `docs/auto-update.md`。核心约束：
//! - 不自动检查，只有用户主动点击才发起网络请求；
//! - GitHub Release 是压缩包，解压出单个可执行文件；
//! - 国内访问 GitHub 不稳定，默认走一组镜像且镜像失效可回退、可自定义。
//!
//! 本模块只做「纯逻辑 + 网络 + 落地到 .new」，UI 对话框在 `editor` 侧驱动。
//! 网络函数均为 blocking，调用方必须放在后台线程（`cx.spawn` + `smol::spawn_blocking`
//! 或独立线程）执行，避免阻塞渲染帧。

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

/// 当前程序版本（来自 Cargo.toml，如 `0.2.4`）。
pub(crate) const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

const REPO_API_LATEST: &str = "https://api.github.com/repos/launcher-rs/rumark/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/launcher-rs/rumark/releases/latest";

/// 内置镜像前缀（按实测可用性排序）。空字符串表示直连，放最后兜底。
/// 实测 2026-11-21：ghfast.top / ghproxy.net 可用；github.moeyy.xyz 已失效。
pub(crate) const DEFAULT_MIRRORS: &[&str] = &["https://ghfast.top", "https://ghproxy.net", ""];

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const USER_AGENT: &str = "rumark-updater/1.0";
/// 下载响应体上限 200MB，防止错配 URL 把超大内容读进内存。
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

/// GitHub Release 的最小化结构（只解析用到的字段）。
#[derive(Deserialize, Debug, Clone)]
pub(crate) struct ReleaseInfo {
    pub(crate) tag_name: String,
    #[serde(default)]
    pub(crate) body: String,
    #[serde(default)]
    pub(crate) published_at: String,
    pub(crate) assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct ReleaseAsset {
    pub(crate) name: String,
    /// 资产大小，仅用于解析完整性（下载时按实际字节校验），字段保留供诊断。
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) size: u64,
    pub(crate) browser_download_url: String,
}

/// 语义版本（major.minor.patch），忽略预发布/构建后缀。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SemVer {
    pub(crate) major: u64,
    pub(crate) minor: u64,
    pub(crate) patch: u64,
}

impl SemVer {
    pub(crate) fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// 当前程序版本；Cargo.toml 的版本恒定合法，失败时退回 0.0.0。
    pub(crate) fn current() -> Self {
        parse_version(CURRENT_VERSION).unwrap_or(Self::new(0, 0, 0))
    }

    pub(crate) fn version_string(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// 解析 `v0.2.5` / `0.2.5-beta.1` 这类版本串；失败返回 None。
pub(crate) fn parse_version(raw: &str) -> Option<SemVer> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    // 去掉预发布 / 构建元数据后缀，只留 x.y.z。
    let core = trimmed.split(['-', '+']).next().unwrap_or("");
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some(SemVer {
        major,
        minor,
        patch,
    })
}

/// 远端版本是否比当前版本新。
pub(crate) fn is_newer(remote: &SemVer, current: &SemVer) -> bool {
    remote > current
}

/// 当前平台对应的 Release 资产文件名。
pub(crate) fn asset_name_for_platform() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "rumark-windows-x86_64.zip"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "rumark-linux-x86_64.tar.gz"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "rumark-macos-aarch64.tar.gz"
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    {
        "rumark-unknown.zip"
    }
}

/// 可执行文件在压缩包内的名字。
pub(crate) fn executable_name_in_archive() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "rumark.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "rumark"
    }
}

/// 把镜像前缀套到原始 URL 外：`mirror` + `/` + `original`。
/// 空前缀（直连）原样返回。对 mirror 末尾的 `/` 做规范化。
pub(crate) fn mirror_url(mirror: &str, original: &str) -> String {
    let mirror = mirror.trim();
    if mirror.is_empty() {
        return original.to_string();
    }
    format!("{}/{}", mirror.trim_end_matches('/'), original)
}

/// 构造用于检查版本的 URL 列表：自定义镜像优先，其次内置默认，最后直连。
/// `custom_mirrors` 每行一个前缀，空行忽略。
pub(crate) fn candidate_mirrors(custom_mirrors: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for line in custom_mirrors {
        let m = line.trim();
        if !m.is_empty() && !result.iter().any(|x| x == m) {
            result.push(m.to_string());
        }
    }
    for &m in DEFAULT_MIRRORS {
        if !result.iter().any(|x| x == m) {
            result.push(m.to_string());
        }
    }
    result
}

/// 在独立线程执行阻塞任务并把结果送回 async 上下文。
/// 避免阻塞 rgpui 的异步执行器；不引入新的 crate 依赖。
pub(crate) async fn run_blocking<T, F>(job: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::Builder::new()
        .name("rumark-update".to_string())
        .spawn(move || {
            let _ = tx.send(job());
        })
        .expect("spawn update worker thread");
    rx.await
        .expect("update worker thread finished without result")
}

fn blocking_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()?)
}

/// 解析 GitHub API 的 Release JSON（纯函数，便于测试）。
pub(crate) fn parse_release(body: &str) -> Result<ReleaseInfo> {
    serde_json::from_str(body).context("解析 GitHub Release 响应失败")
}

/// 查询最新版本，依次尝试各镜像。返回 (Release 信息, 命中的镜像前缀)。
/// 必须在后台线程调用。
pub(crate) fn fetch_latest_release(custom_mirrors: &[String]) -> Result<(ReleaseInfo, String)> {
    let client = blocking_client()?;
    let mirrors = candidate_mirrors(custom_mirrors);
    let mut attempts = Vec::new();
    for mirror in &mirrors {
        let url = mirror_url(mirror, REPO_API_LATEST);
        match client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.text())
        {
            Ok(body) => match parse_release(&body) {
                Ok(release) => return Ok((release, mirror.clone())),
                Err(err) => attempts.push(format!("{url}: {err}")),
            },
            Err(err) => attempts.push(format!("{url}: {err}")),
        }
    }
    bail!("检查更新失败，已尝试：\n{}", attempts.join("\n"))
}

/// 在 release 的资产里找当前平台对应的那一个。
pub(crate) fn find_asset<'a>(release: &'a ReleaseInfo, name: &str) -> Result<&'a ReleaseAsset> {
    release
        .assets
        .iter()
        .find(|a| a.name == name)
        .with_context(|| format!("Release 中未找到资产 {name}"))
}

/// 从下载结果里解压出可执行文件，写入 `dest_dir/<name>.new`。
/// 返回落地的临时可执行文件路径。必须在后台线程调用。
pub(crate) fn download_and_stage(
    asset: &ReleaseAsset,
    custom_mirrors: &[String],
    dest_dir: &Path,
) -> Result<PathBuf> {
    let client = blocking_client()?;
    let mirrors = candidate_mirrors(custom_mirrors);
    let mut attempts = Vec::new();
    let mut bytes: Option<Vec<u8>> = None;
    for mirror in &mirrors {
        let url = mirror_url(mirror, &asset.browser_download_url);
        match client
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
        {
            Ok(chunk) if !chunk.is_empty() && (chunk.len() as u64) <= MAX_DOWNLOAD_BYTES => {
                bytes = Some(chunk.to_vec());
                break;
            }
            Ok(chunk) => attempts.push(format!("{url}: 响应大小异常 {} 字节", chunk.len())),
            Err(err) => attempts.push(format!("{url}: {err}")),
        }
    }
    let bytes = bytes.with_context(|| format!("下载失败，已尝试：\n{}", attempts.join("\n")))?;

    let exe_name = executable_name_in_archive();
    let mut payload: Option<Vec<u8>> = None;

    // Windows / 部分平台是 zip。
    if asset.name.ends_with(".zip") {
        let cursor = std::io::Cursor::new(&bytes);
        let mut archive = zip::ZipArchive::new(cursor).context("打开的更新包不是合法 zip")?;
        let mut file = archive
            .by_name(exe_name)
            .with_context(|| format!("更新包内未找到 {exe_name}"))?;
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)?;
        payload = Some(buf);
    }

    // Linux / macOS 是 tar.gz（仅按扩展名区分，避免引入额外 tar 依赖时误判）。
    if payload.is_none() && asset.name.ends_with(".tar.gz") {
        bail!("Linux/macOS 的 tar.gz 更新包解包尚未启用，请从发布页手动下载");
    }

    let payload = payload.context("无法识别的更新包格式")?;
    if payload.is_empty() {
        bail!("更新包内的可执行文件为空");
    }

    std::fs::create_dir_all(dest_dir)?;
    let staged = dest_dir.join(format!("{exe_name}.new"));
    let mut out = std::fs::File::create(&staged)?;
    out.write_all(&payload)?;
    out.flush()?;
    Ok(staged)
}

/// 打开系统浏览器到 releases 页面（用平台默认命令，失败返回错误）。
pub(crate) fn open_release_page() -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", RELEASES_PAGE])
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(RELEASES_PAGE)
            .spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(RELEASES_PAGE)
            .spawn()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_semver_with_v_prefix_and_suffix() {
        assert_eq!(parse_version("v0.2.5"), Some(SemVer::new(0, 2, 5)));
        assert_eq!(parse_version("0.3.0-beta.1"), Some(SemVer::new(0, 3, 0)));
        assert_eq!(parse_version("1.2.3+build.9"), Some(SemVer::new(1, 2, 3)));
        assert_eq!(parse_version("garbage"), None);
        assert_eq!(parse_version("v1.2"), None);
    }

    #[test]
    fn compares_versions() {
        let cur = SemVer::new(0, 2, 4);
        assert!(is_newer(&SemVer::new(0, 2, 5), &cur));
        assert!(is_newer(&SemVer::new(1, 0, 0), &cur));
        assert!(!is_newer(&SemVer::new(0, 2, 4), &cur));
        assert!(!is_newer(&SemVer::new(0, 2, 3), &cur));
    }

    #[test]
    fn mirrors_are_normalized_and_deduped() {
        assert_eq!(
            mirror_url("https://ghfast.top/", "https://github.com/x"),
            "https://ghfast.top/https://github.com/x"
        );
        assert_eq!(
            mirror_url("", "https://github.com/x"),
            "https://github.com/x"
        );
        let custom = vec![
            " https://a.com ".to_string(),
            "".to_string(),
            "https://a.com".to_string(),
        ];
        let list = candidate_mirrors(&custom);
        assert_eq!(list[0], "https://a.com");
        assert_eq!(list.iter().filter(|x| *x == "https://a.com").count(), 1);
        // 内置默认 + 直连兜底一定在列表里。
        assert!(list.contains(&"https://ghfast.top".to_string()));
        assert!(list.contains(&String::new()));
    }

    const SAMPLE_RELEASE: &str = r#"{
        "tag_name": "v0.2.5",
        "name": "拾墨 Rumark v0.2.5",
        "body": "修复若干问题",
        "published_at": "2026-11-22T08:00:00Z",
        "assets": [
            {"name": "rumark-windows-x86_64.zip", "size": 123, "browser_download_url": "https://github.com/launcher-rs/rumark/releases/download/v0.2.5/rumark-windows-x86_64.zip"}
        ]
    }"#;

    #[test]
    fn parses_release_and_finds_asset() {
        let release = parse_release(SAMPLE_RELEASE).unwrap();
        assert_eq!(release.tag_name, "v0.2.5");
        let asset = find_asset(&release, "rumark-windows-x86_64.zip").unwrap();
        assert_eq!(asset.size, 123);
        assert!(find_asset(&release, "missing.zip").is_err());
    }

    #[test]
    fn asset_name_is_platform_specific() {
        let name = asset_name_for_platform();
        assert!(name.starts_with("rumark-"));
        assert!(name.contains(std::env::consts::OS));
    }
}
