//! rumark 的更新配置：应用专属信息与偏好在此注入，通用逻辑来自 `rgpui-extra`。
//!
//! 组件仓库位于 `../rgpui-extra`，launcher-rs 的其他程序直接依赖它即可，
//! 无需复制版本比较 / 镜像回退 / 分块下载 / 解压落地代码。

use rgpui_extra::updater::{Config, Provider};

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

/// 内置镜像列表（供偏好页展示/一键填入）。
pub(crate) const DEFAULT_MIRRORS: &[&str] = rgpui_extra::updater::DEFAULT_MIRRORS;

pub(crate) use rgpui_extra::updater::{
    DownloadProgress, InstallOptions, ReleaseAsset, check, download_and_stage, load_staged_update,
    open_release_page, run_blocking, spawn_install_helper,
};

/// 构造 rumark 的更新配置。镜像默认关闭，只有偏好里显式开启才走中转。
pub(crate) fn updater_config(custom_mirrors: Vec<String>, use_mirrors: bool) -> Config {
    let mut config = Config::new("launcher-rs/rumark", "rumark");
    config.provider = updater_provider();
    config.user_agent = "rumark-updater/1.0".to_string();
    config.custom_mirrors = custom_mirrors;
    config.use_mirrors = use_mirrors;
    config
}

#[cfg(test)]
mod tests {
    use super::*;

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
