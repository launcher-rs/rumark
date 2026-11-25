//! rumark 的更新配置：应用专属信息与偏好在此注入，通用逻辑来自 `rgpui-extra`。
//!
//! 组件仓库位于 `../rgpui-extra`，launcher-rs 的其他程序直接依赖它即可，
//! 无需复制版本比较 / 镜像回退 / 分块下载 / 解压落地代码。

use std::time::Duration;

use anyhow::anyhow;
use rgpui_extra::updater::{Config, Provider, ReleaseInfo, fetch_latest_release, mirror_url};

/// 当前程序版本（来自 Cargo.toml）。
pub(crate) const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// rumark 的发布源平台。换发布平台时只改这里，其余更新逻辑无需改动。
///
/// 常用写法：
/// - GitHub：`Provider::GitHub`
/// - Gitee：`Provider::Gitee`
/// - GitLab.com：`Provider::GitLab { base_url: "https://gitlab.com".into() }`
/// - 自托管 GitLab：`Provider::GitLab { base_url: "https://git.example.com".into() }`
/// - Gitea / Forgejo：`Provider::Custom { api_url, releases_page }`
pub(crate) fn updater_provider() -> Provider {
    Provider::GitHub
}

/// 实测可用的 GitHub 镜像前缀（2026-11-25，与 RuTerm 共用同一组结论）。
///
/// - `gh-proxy.com` / `gh-proxy.org`：Release API 返回合法 JSON，安装包支持 Range 下载；
/// - `ghfast.top` / `ghproxy.net`：安装包可用，API 返回 403；直连优先后只在回退阶段用到下载能力。
///
/// 改动前先按「Release API 合法 JSON + 安装包 206/ZIP 头」两项重新实测。
pub(crate) const RECOMMENDED_MIRRORS: &[&str] = &[
    "https://gh-proxy.com",
    "https://gh-proxy.org",
    "https://ghfast.top",
    "https://ghproxy.net",
];

/// 新配置默认填入的镜像前缀。
pub(crate) fn recommended_mirrors() -> Vec<String> {
    RECOMMENDED_MIRRORS
        .iter()
        .map(|mirror| (*mirror).to_string())
        .collect()
}

pub(crate) use rgpui_extra::updater::{
    DownloadProgress, InstallOptions, ReleaseAsset, check, download_and_stage, load_staged_update,
    open_release_page, run_blocking, spawn_install_helper,
};

/// 构造 rumark 的更新配置。
pub(crate) fn updater_config(custom_mirrors: Vec<String>, use_mirrors: bool) -> Config {
    let mut config = Config::new("launcher-rs/rumark", "rumark");
    config.provider = updater_provider();
    config.user_agent = "rumark-updater/1.0".to_string();
    config.custom_mirrors = custom_mirrors;
    config.use_mirrors = use_mirrors;
    config
}

/// 实际检查/下载用的候选配置：官方直连永远排第一；直连失败且用户开启镜像回退，
/// 才追加一组按自定义前缀 → 组件内置镜像回退的配置。
pub(crate) fn fallback_configs(custom_mirrors: Vec<String>, use_mirrors: bool) -> Vec<Config> {
    let mut configs = vec![updater_config(Vec::new(), false)];
    if use_mirrors {
        configs.push(updater_config(custom_mirrors, true));
    }
    configs
}

/// 依次尝试候选配置，返回第一个成功结果；全失败时保留最后一个错误。
pub(crate) fn try_configs<T, F>(configs: Vec<Config>, mut job: F) -> anyhow::Result<T>
where
    F: FnMut(&Config) -> anyhow::Result<T>,
{
    let mut last_error = None;
    for config in &configs {
        match job(config) {
            Ok(value) => return Ok(value),
            Err(error) => {
                eprintln!("更新通道失败，尝试下一个：{error}");
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("没有可用的更新通道")))
}

/// 单个镜像的探测结果；`mirror` 为空表示官方直连。
#[derive(Clone, Debug)]
pub(crate) struct MirrorProbe {
    pub(crate) mirror: String,
    pub(crate) api_ok: bool,
    pub(crate) download_ok: bool,
    pub(crate) detail: String,
}

/// 逐个探测候选镜像：Release API 是否返回合法 JSON、安装包是否支持首字节下载。
/// 必须在后台线程调用；超时压到 12 秒，避免失效站点拖住整个列表。
pub(crate) fn probe_mirrors(config: &Config) -> Vec<MirrorProbe> {
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(6))
        .timeout(Duration::from_secs(12))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(&config.user_agent)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return vec![MirrorProbe {
                mirror: String::new(),
                api_ok: false,
                download_ok: false,
                detail: error.to_string(),
            }];
        }
    };

    // 先从任一通道拿到安装包地址；拿不到时只测 API，不误报下载失败。
    let asset_url = fetch_latest_release(config).ok().and_then(|(release, _)| {
        release
            .assets
            .into_iter()
            .find(|asset| asset.name == config.asset_name())
            .map(|asset| asset.browser_download_url)
    });

    // 无论镜像开关是否打开，都列出全部候选；直连排在最前。
    let mut probe_config = config.clone();
    probe_config.use_mirrors = true;
    let api_url = config.api_url();

    let mut results: Vec<MirrorProbe> = probe_config
        .candidate_mirrors()
        .into_iter()
        .map(|mirror| {
            let mut errors = Vec::new();

            let api_ok = match client
                .get(mirror_url(&mirror, &api_url))
                .header("Accept", "application/vnd.github+json")
                .send()
            {
                Ok(response) => {
                    let status = response.status();
                    match response.text() {
                        Ok(body)
                            if status.is_success()
                                && ReleaseInfo::parse_for(&config.provider, &body).is_ok() =>
                        {
                            true
                        }
                        Ok(_) => {
                            errors.push(format!("API HTTP {status}"));
                            false
                        }
                        Err(error) => {
                            errors.push(format!("API {error}"));
                            false
                        }
                    }
                }
                Err(error) => {
                    errors.push(format!("API {error}"));
                    false
                }
            };

            let download_ok = if let Some(asset) = &asset_url {
                match client
                    .get(mirror_url(&mirror, asset))
                    .header("Range", "bytes=0-1023")
                    .send()
                {
                    Ok(response) => {
                        let status = response.status();
                        if status.is_success() || status == reqwest::StatusCode::PARTIAL_CONTENT {
                            match response.bytes() {
                                Ok(bytes) => !bytes.is_empty(),
                                Err(error) => {
                                    errors.push(format!("下载 {error}"));
                                    false
                                }
                            }
                        } else {
                            errors.push(format!("下载 HTTP {status}"));
                            false
                        }
                    }
                    Err(error) => {
                        errors.push(format!("下载 {error}"));
                        false
                    }
                }
            } else {
                false
            };

            MirrorProbe {
                mirror,
                api_ok,
                download_ok,
                detail: errors.join("；"),
            }
        })
        .collect();
    results.sort_by_key(|probe| if probe.mirror.is_empty() { 0 } else { 1 });
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommended_mirrors_match_ruterm_and_direct_is_first() {
        assert_eq!(recommended_mirrors().len(), 4);
        assert_eq!(RECOMMENDED_MIRRORS[0], "https://gh-proxy.com");

        let only_direct = fallback_configs(recommended_mirrors(), false);
        assert_eq!(only_direct.len(), 1);
        assert_eq!(only_direct[0].candidate_mirrors(), vec![String::new()]);

        let with_fallback = fallback_configs(recommended_mirrors(), true);
        assert_eq!(with_fallback.len(), 2);
        assert_eq!(with_fallback[0].candidate_mirrors(), vec![String::new()]);
        assert_eq!(
            with_fallback[1].candidate_mirrors()[0],
            RECOMMENDED_MIRRORS[0]
        );
    }

    #[test]
    fn try_configs_stops_at_first_success() {
        let configs = fallback_configs(vec![], false);
        let mut seen = 0;
        let result = try_configs(configs, |_| {
            seen += 1;
            Ok::<_, anyhow::Error>(seen)
        });
        assert_eq!(result.unwrap(), 1);
        assert_eq!(seen, 1);
    }

    #[test]
    #[ignore = "需要外网，手动运行以查看镜像实时状态"]
    fn probe_mirrors_reports_each_candidate() {
        let config = updater_config(recommended_mirrors(), true);
        let results = probe_mirrors(&config);
        assert!(!results.is_empty());
        assert_eq!(results[0].mirror, "", "直连必须排在最前");
        for result in &results {
            println!(
                "mirror={:?} api={} download={} detail={}",
                result.mirror, result.api_ok, result.download_ok, result.detail
            );
        }
    }

    #[test]
    fn config_uses_rumark_repository_and_defaults_to_direct() {
        let config = updater_config(vec!["https://mirror.example".into()], false);
        assert_eq!(
            config.api_url(),
            "https://api.github.com/repos/launcher-rs/rumark/releases/latest"
        );
        assert_eq!(config.candidate_mirrors(), vec![String::new()]);

        let config = updater_config(vec!["https://mirror.example".into()], true);
        assert_eq!(config.candidate_mirrors()[0], "https://mirror.example");
    }

    #[test]
    fn config_supports_switching_release_provider() {
        let mut config = updater_config(vec![], false);
        assert_eq!(config.provider, Provider::GitHub);
        assert_eq!(
            config.api_url(),
            "https://api.github.com/repos/launcher-rs/rumark/releases/latest"
        );

        config.provider = Provider::Gitee;
        assert_eq!(
            config.api_url(),
            "https://gitee.com/api/v5/repos/launcher-rs/rumark/releases/latest"
        );

        config.provider = Provider::GitLab {
            base_url: "https://gitlab.com".into(),
        };
        assert_eq!(
            config.api_url(),
            "https://gitlab.com/api/v4/projects/launcher-rs%2Frumark/releases/permalink/latest"
        );
        assert_eq!(
            config.releases_page(),
            "https://gitlab.com/launcher-rs/rumark/-/releases"
        );
    }
}
