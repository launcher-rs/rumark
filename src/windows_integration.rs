//! 仅在用户点击时登记打开能力；默认应用由用户在系统设置中确认。

#![cfg(windows)]

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE, REG_NONE};
use winreg::types::ToRegValue as _;
use winreg::{RegKey, RegValue};

pub(crate) const DEFAULT_APPS_URI: &str = "ms-settings:defaultapps?registeredAppUser=Rumark";

const PROG_ID: &str = "rs.launcher.Rumark.Markdown";
const CAPABILITIES: &str = r"Software\Rumark\Capabilities";

pub(crate) fn register_markdown_application(executable: &Path, description: &str) -> Result<()> {
    let plan = registration_plan(executable, description)?;
    apply_registration_plan(plan)
}

/// 在 `.md` / `.markdown` 的资源管理器右键菜单中登记「用 Rumark 打开」。
pub(crate) fn register_explorer_context_menu(executable: &Path, label: &str) -> Result<()> {
    let plan = context_menu_plan(executable, label)?;
    apply_registration_plan(plan)
}

fn apply_registration_plan(plan: Vec<RegistrationKey>) -> Result<()> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    for entry in plan {
        let (key, _) = current_user
            .create_subkey_with_flags(entry.path, KEY_SET_VALUE)
            .with_context(|| format!("create HKCU\\{}", entry.path))?;
        for (name, value) in entry.values {
            key.set_raw_value(name, &value)
                .with_context(|| format!("write HKCU\\{} value {name:?}", entry.path))?;
        }
    }
    Ok(())
}

struct RegistrationKey {
    path: &'static str,
    values: Vec<(&'static str, RegValue<'static>)>,
}

fn owned_reg_value(value: &impl winreg::types::ToRegValue) -> RegValue<'static> {
    let value = value.to_reg_value();
    RegValue {
        bytes: value.bytes.into_owned().into(),
        vtype: value.vtype,
    }
}

/// 校验可执行文件路径并返回应用图标、文档图标和打开命令。
///
/// 全量校验在写入注册表之前完成，避免无效输入造成部分登记。
fn validated_commands(executable: &Path) -> Result<(OsString, OsString, OsString)> {
    ensure!(
        executable.is_absolute() && executable.file_name().is_some(),
        "executable path must be absolute and include a file name"
    );
    ensure!(
        !executable
            .as_os_str()
            .encode_wide()
            .any(|unit| unit == 0 || unit == u16::from(b'"')),
        "executable path contains a NUL or quote"
    );

    // 保留原始 Windows 路径，不经 UTF-8 或有损转换。
    let mut quoted_executable = OsString::from("\"");
    quoted_executable.push(executable.as_os_str());
    quoted_executable.push("\"");
    let mut app_icon = quoted_executable.clone();
    app_icon.push(",0");
    let mut document_icon = quoted_executable.clone();
    document_icon.push(",1");
    let mut command = quoted_executable;
    command.push(" \"%1\"");
    Ok((app_icon, document_icon, command))
}

fn registration_plan(executable: &Path, description: &str) -> Result<Vec<RegistrationKey>> {
    ensure!(
        !description.contains('\0'),
        "application description contains a NUL"
    );
    let (app_icon, document_icon, command) = validated_commands(executable)?;

    Ok(vec![
        RegistrationKey {
            path: r"Software\Classes\rs.launcher.Rumark.Markdown",
            values: vec![("", owned_reg_value(&description))],
        },
        RegistrationKey {
            path: r"Software\Classes\rs.launcher.Rumark.Markdown\DefaultIcon",
            values: vec![("", owned_reg_value(&document_icon))],
        },
        RegistrationKey {
            path: r"Software\Classes\rs.launcher.Rumark.Markdown\shell\open\command",
            values: vec![("", owned_reg_value(&command))],
        },
        RegistrationKey {
            path: r"Software\Classes\.md\OpenWithProgids",
            values: vec![(
                PROG_ID,
                RegValue {
                    bytes: Vec::new().into(),
                    vtype: REG_NONE,
                },
            )],
        },
        RegistrationKey {
            path: r"Software\Classes\.markdown\OpenWithProgids",
            values: vec![(
                PROG_ID,
                RegValue {
                    bytes: Vec::new().into(),
                    vtype: REG_NONE,
                },
            )],
        },
        RegistrationKey {
            path: CAPABILITIES,
            values: vec![
                ("ApplicationName", "Rumark".to_reg_value()),
                ("ApplicationDescription", owned_reg_value(&description)),
                ("ApplicationIcon", owned_reg_value(&app_icon)),
            ],
        },
        RegistrationKey {
            path: r"Software\Rumark\Capabilities\FileAssociations",
            values: vec![
                (".md", PROG_ID.to_reg_value()),
                (".markdown", PROG_ID.to_reg_value()),
            ],
        },
        RegistrationKey {
            path: r"Software\RegisteredApplications",
            values: vec![("Rumark", CAPABILITIES.to_reg_value())],
        },
    ])
}

const CONTEXT_MENU_KEYS: [(&str, &str); 2] = [
    (
        r"Software\Classes\SystemFileAssociations\.md\shell\Rumark",
        r"Software\Classes\SystemFileAssociations\.md\shell\Rumark\command",
    ),
    (
        r"Software\Classes\SystemFileAssociations\.markdown\shell\Rumark",
        r"Software\Classes\SystemFileAssociations\.markdown\shell\Rumark\command",
    ),
];

fn context_menu_plan(executable: &Path, label: &str) -> Result<Vec<RegistrationKey>> {
    ensure!(
        !label.trim().is_empty(),
        "context menu label must not be empty"
    );
    ensure!(!label.contains('\0'), "context menu label contains a NUL");
    let (_, document_icon, command) = validated_commands(executable)?;

    let mut plan = Vec::with_capacity(CONTEXT_MENU_KEYS.len() * 2);
    for (entry_path, command_path) in CONTEXT_MENU_KEYS {
        plan.push(RegistrationKey {
            path: entry_path,
            values: vec![
                ("", owned_reg_value(&label)),
                ("Icon", owned_reg_value(&document_icon)),
            ],
        });
        plan.push(RegistrationKey {
            path: command_path,
            values: vec![("", owned_reg_value(&command))],
        });
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use std::os::windows::ffi::OsStringExt as _;

    use winreg::enums::REG_SZ;

    use super::*;

    fn value<'a>(plan: &'a [RegistrationKey], path: &str, name: &str) -> &'a RegValue<'static> {
        let entry = plan.iter().find(|entry| entry.path == path).unwrap();
        &entry.values.iter().find(|(key, _)| *key == name).unwrap().1
    }

    #[test]
    fn command_and_icons_quote_spaces_and_unicode_paths() {
        for executable in [
            r"C:\Program Files\Rumark\rumark.exe",
            r"C:\应用 程序\拾墨\rumark.exe",
            r"\\server\共享 目录\Rumark\rumark.exe",
            r"C:\Apps & Tools\100%,中文\rumark.exe",
        ] {
            let plan = registration_plan(Path::new(executable), "Markdown 文档").unwrap();
            assert_eq!(
                value(
                    &plan,
                    r"Software\Classes\rs.launcher.Rumark.Markdown\shell\open\command",
                    ""
                ),
                &format!("\"{executable}\" \"%1\"").to_reg_value()
            );
            let expected_document_icon = format!("\"{executable}\",1").to_reg_value();
            assert_eq!(
                value(
                    &plan,
                    r"Software\Classes\rs.launcher.Rumark.Markdown\DefaultIcon",
                    ""
                ),
                &expected_document_icon
            );
            let expected_app_icon = format!("\"{executable}\",0").to_reg_value();
            assert_eq!(
                value(&plan, CAPABILITIES, "ApplicationIcon"),
                &expected_app_icon
            );
        }
    }

    #[test]
    fn windows_path_code_units_are_preserved() {
        let mut path: Vec<u16> = r"C:\Rumark\".encode_utf16().collect();
        path.push(0xd800);
        path.extend(r"\rumark.exe".encode_utf16());
        let executable = OsString::from_wide(&path);
        assert!(executable.to_str().is_none());
        let plan = registration_plan(Path::new(&executable), "Markdown").unwrap();

        for (key, name, suffix) in [
            (
                r"Software\Classes\rs.launcher.Rumark.Markdown\shell\open\command",
                "",
                "\" \"%1\"",
            ),
            (
                r"Software\Classes\rs.launcher.Rumark.Markdown\DefaultIcon",
                "",
                "\",1",
            ),
            (CAPABILITIES, "ApplicationIcon", "\",0"),
        ] {
            let expected: Vec<u8> = std::iter::once(u16::from(b'"'))
                .chain(path.iter().copied())
                .chain(suffix.encode_utf16())
                .chain(std::iter::once(0))
                .flat_map(u16::to_le_bytes)
                .collect();
            let actual = value(&plan, key, name);
            assert_eq!(actual.vtype, REG_SZ);
            assert_eq!(actual.bytes, expected);
        }
    }

    #[test]
    fn plan_contains_only_required_keys_names_and_types() {
        let description = "Markdown 文档";
        let plan = registration_plan(Path::new(r"C:\Rumark\rumark.exe"), description).unwrap();
        let actual: Vec<_> = plan
            .iter()
            .flat_map(|entry| {
                entry
                    .values
                    .iter()
                    .map(move |(name, value)| (entry.path, *name, value.vtype.clone()))
            })
            .collect();
        assert_eq!(
            actual,
            vec![
                (r"Software\Classes\rs.launcher.Rumark.Markdown", "", REG_SZ),
                (
                    r"Software\Classes\rs.launcher.Rumark.Markdown\DefaultIcon",
                    "",
                    REG_SZ
                ),
                (
                    r"Software\Classes\rs.launcher.Rumark.Markdown\shell\open\command",
                    "",
                    REG_SZ
                ),
                (
                    r"Software\Classes\.md\OpenWithProgids",
                    "rs.launcher.Rumark.Markdown",
                    REG_NONE
                ),
                (
                    r"Software\Classes\.markdown\OpenWithProgids",
                    "rs.launcher.Rumark.Markdown",
                    REG_NONE
                ),
                (r"Software\Rumark\Capabilities", "ApplicationName", REG_SZ),
                (
                    r"Software\Rumark\Capabilities",
                    "ApplicationDescription",
                    REG_SZ
                ),
                (r"Software\Rumark\Capabilities", "ApplicationIcon", REG_SZ),
                (
                    r"Software\Rumark\Capabilities\FileAssociations",
                    ".md",
                    REG_SZ
                ),
                (
                    r"Software\Rumark\Capabilities\FileAssociations",
                    ".markdown",
                    REG_SZ
                ),
                (r"Software\RegisteredApplications", "Rumark", REG_SZ),
            ]
        );
        for entry in &plan {
            assert!(!entry.path.to_ascii_lowercase().contains("userchoice"));
            if entry.path.starts_with(r"Software\Classes\.") {
                assert!(entry.path.ends_with(r"\OpenWithProgids"));
                for (name, value) in &entry.values {
                    assert_eq!(*name, PROG_ID);
                    assert_eq!(value.vtype, REG_NONE);
                    assert!(value.bytes.is_empty());
                }
            }
        }
        for (key, name, expected) in [
            (
                r"Software\Classes\rs.launcher.Rumark.Markdown",
                "",
                description,
            ),
            (CAPABILITIES, "ApplicationName", "Rumark"),
            (CAPABILITIES, "ApplicationDescription", description),
            (
                r"Software\Rumark\Capabilities\FileAssociations",
                ".md",
                PROG_ID,
            ),
            (
                r"Software\Rumark\Capabilities\FileAssociations",
                ".markdown",
                PROG_ID,
            ),
            (r"Software\RegisteredApplications", "Rumark", CAPABILITIES),
        ] {
            assert_eq!(value(&plan, key, name), &expected.to_reg_value());
        }
        assert_eq!(
            DEFAULT_APPS_URI,
            "ms-settings:defaultapps?registeredAppUser=Rumark"
        );
    }

    #[test]
    fn context_menu_plan_targets_markdown_extensions_without_touching_defaults() {
        let label = "用 Rumark 打开";
        let plan = context_menu_plan(Path::new(r"C:\Program Files\Rumark\rumark.exe"), label)
            .expect("valid executable should produce a plan");

        for (entry_path, command_path) in CONTEXT_MENU_KEYS {
            assert_eq!(value(&plan, entry_path, ""), &label.to_reg_value());
            assert_eq!(
                value(&plan, entry_path, "Icon"),
                &r#""C:\Program Files\Rumark\rumark.exe",1"#.to_reg_value()
            );
            assert_eq!(
                value(&plan, command_path, ""),
                &r#""C:\Program Files\Rumark\rumark.exe" "%1""#.to_reg_value()
            );
        }
        assert_eq!(plan.len(), CONTEXT_MENU_KEYS.len() * 2);
        for entry in &plan {
            assert!(!entry.path.to_ascii_lowercase().contains("userchoice"));
            assert!(!entry.path.to_ascii_lowercase().contains("openwithprogids"));
        }
    }

    #[test]
    fn context_menu_plan_rejects_empty_or_invalid_labels() {
        let executable = Path::new(r"C:\Rumark\rumark.exe");
        for label in ["", "   ", "bad\0label"] {
            assert!(context_menu_plan(executable, label).is_err());
        }
        assert!(context_menu_plan(Path::new(r"rumark.exe"), "Open with Rumark").is_err());
    }

    #[test]
    fn invalid_inputs_are_rejected_before_registry_access() {
        for executable in [
            "",
            r"rumark.exe",
            r"C:rumark.exe",
            r"C:\",
            "C:\\Rumark\\bad\"name.exe",
            "C:\\Rumark\\bad\0name.exe",
        ] {
            assert!(registration_plan(Path::new(executable), "Markdown").is_err());
        }
        assert!(registration_plan(Path::new(r"C:\Rumark\rumark.exe"), "bad\0description").is_err());
    }
}
