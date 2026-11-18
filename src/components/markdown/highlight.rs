//! 代码块语法高亮支持。

#[cfg(feature = "code-highlight-core")]
use std::collections::HashMap;
use std::ops::Range;
#[cfg(feature = "code-highlight-core")]
use std::sync::LazyLock;

use rgpui::Hsla;
#[cfg(feature = "code-highlight-core")]
use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

/// 语法高亮注册表使用的规范语言键。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HighlightLanguage {
    /// Rust 源码。
    Rust,
    /// 不含 JSX 的 JavaScript。
    JavaScript,
    /// 含 JSX 语法的 JavaScript。
    JavaScriptJsx,
    /// 不含 TSX 的 TypeScript。
    TypeScript,
    /// 含 TSX 语法的 TypeScript。
    TypeScriptTsx,
    /// JSON 数据。
    Json,
    /// Markdown 源码。
    Markdown,
    /// 类 POSIX Shell 脚本。
    Bash,
    /// C 源码。
    C,
    /// C++ 源码。
    Cpp,
    /// C# 源码。
    CSharp,
    /// CSS 样式表。
    Css,
    /// Go 源码。
    Go,
    /// HTML 标记。
    Html,
    /// Java 源码。
    Java,
    /// PHP 源码。
    Php,
    /// Python 源码。
    Python,
    /// Ruby 源码。
    Ruby,
    /// YAML 配置。
    Yaml,
    /// TOML 配置。
    Toml,
    /// Mermaid 图表源码。
    Mermaid,
    /// 纯文本或未知语言回退。
    PlainText,
}

/// 映射到主题色的语义高亮类别。
///
/// 无 tree-sitter 核心时不会构造任何类别（高亮结果恒为空），因此放开未构造告警。
#[cfg_attr(not(feature = "code-highlight-core"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HighlightClass {
    /// 注释文本。
    Comment,
    /// 语言关键字或保留字。
    Keyword,
    /// 字符串字面量。
    String,
    /// 数字字面量。
    Number,
    /// 类型名。
    Type,
    /// 函数或可调用标识。
    Function,
    /// 常量标识。
    Constant,
    /// 变量标识。
    Variable,
    /// 对象或记录属性。
    Property,
    /// 运算符。
    Operator,
    /// 标点。
    Punctuation,
}

/// 代码块内的已高亮字节范围。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HighlightSpan {
    pub(crate) range: Range<usize>,
    pub(crate) class: HighlightClass,
}

/// 缓存在代码块上的高亮结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HighlightResult {
    pub(crate) language: HighlightLanguage,
    pub(crate) spans: Vec<HighlightSpan>,
}

/// 围栏代码信息串接受的语言别名。
#[derive(Clone, Copy)]
struct HighlightLanguageSpec {
    key: HighlightLanguage,
    aliases: &'static [&'static str],
}

const HIGHLIGHT_LANGUAGES: &[HighlightLanguageSpec] = &[
    HighlightLanguageSpec {
        key: HighlightLanguage::Rust,
        aliases: &["rust", "rs"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::JavaScript,
        aliases: &["javascript", "js"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::JavaScriptJsx,
        aliases: &["jsx"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::TypeScript,
        aliases: &["typescript", "ts"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::TypeScriptTsx,
        aliases: &["tsx"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Json,
        aliases: &["json"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Markdown,
        aliases: &["markdown", "md"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Bash,
        aliases: &["bash", "sh", "shell", "zsh"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::C,
        aliases: &["c", "h"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Cpp,
        aliases: &["cpp", "cxx", "cc", "hpp", "hxx"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::CSharp,
        aliases: &["csharp", "cs", "c#"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Css,
        aliases: &["css"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Go,
        aliases: &["go", "golang"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Html,
        aliases: &["html"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Java,
        aliases: &["java"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Php,
        aliases: &["php"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Python,
        aliases: &["python", "py"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Ruby,
        aliases: &["ruby", "rb"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Yaml,
        aliases: &["yaml", "yml"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Toml,
        aliases: &["toml"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::PlainText,
        aliases: &["text", "txt", "plain"],
    },
    HighlightLanguageSpec {
        key: HighlightLanguage::Mermaid,
        aliases: &["mermaid", "mmd"],
    },
];

/// 返回输入前缀匹配的首个高亮语言名称，用于代码围栏语言补全。
pub(crate) fn suggest_code_language(prefix: &str) -> Option<&'static str> {
    let prefix = prefix.trim().to_ascii_lowercase();
    if prefix.is_empty() {
        return None;
    }

    HIGHLIGHT_LANGUAGES
        .iter()
        .flat_map(|language| language.aliases.iter().copied())
        .find(|alias| alias.starts_with(&prefix))
}

/// tree-sitter 标签名表，顺序与官方 highlightr 的 tags.json 一致。
#[cfg(feature = "code-highlight-core")]
const HIGHLIGHT_TAG_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "function",
    "function.builtin",
    "keyword",
    "module",
    "number",
    "operator",
    "property",
    "property.builtin",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.special",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

/// 懒构建的 tree-sitter 高亮注册表。
#[cfg(feature = "code-highlight-core")]
struct HighlightRegistry {
    configs: HashMap<HighlightLanguage, HighlightConfiguration>,
}

#[cfg(feature = "code-highlight-core")]
static CODE_HIGHLIGHT_REGISTRY: LazyLock<HighlightRegistry> = LazyLock::new(HighlightRegistry::new);

#[cfg(feature = "code-highlight-core")]
impl HighlightRegistry {
    /// 新建注册表。
    fn new() -> Self {
        let mut configs = HashMap::new();
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::Rust, build_rust_config());
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(
            &mut configs,
            HighlightLanguage::JavaScript,
            build_javascript_config(),
        );
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(
            &mut configs,
            HighlightLanguage::JavaScriptJsx,
            build_jsx_config(),
        );
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(
            &mut configs,
            HighlightLanguage::TypeScript,
            build_typescript_config(),
        );
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(
            &mut configs,
            HighlightLanguage::TypeScriptTsx,
            build_tsx_config(),
        );
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::Json, build_json_config());
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(
            &mut configs,
            HighlightLanguage::Markdown,
            build_markdown_config(),
        );
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::Bash, build_bash_config());
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::C, build_c_config());
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::Cpp, build_cpp_config());
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(
            &mut configs,
            HighlightLanguage::CSharp,
            build_csharp_config(),
        );
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::Css, build_css_config());
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::Go, build_go_config());
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::Html, build_html_config());
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::Java, build_java_config());
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::Php, build_php_config());
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(
            &mut configs,
            HighlightLanguage::Python,
            build_python_config(),
        );
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::Ruby, build_ruby_config());
        #[cfg(feature = "code-highlight-official")]
        maybe_insert_config(&mut configs, HighlightLanguage::Yaml, build_yaml_config());
        #[cfg(feature = "code-highlight-config")]
        maybe_insert_config(&mut configs, HighlightLanguage::Toml, build_toml_config());
        Self { configs }
    }

    /// 取指定语言的高亮配置。
    fn config_for(&self, key: HighlightLanguage) -> Option<&HighlightConfiguration> {
        self.configs.get(&key)
    }
}

/// 有配置时插入注册表。
#[cfg(feature = "code-highlight-core")]
fn maybe_insert_config(
    configs: &mut HashMap<HighlightLanguage, HighlightConfiguration>,
    key: HighlightLanguage,
    config: Option<HighlightConfiguration>,
) {
    if let Some(config) = config {
        configs.insert(key, config);
    }
}

/// 用查询语句构造高亮配置。
#[cfg(feature = "code-highlight-core")]
fn configure_highlights(
    language: tree_sitter::Language,
    name: &'static str,
    highlights_query: &str,
    injections_query: &str,
    locals_query: &str,
) -> Option<HighlightConfiguration> {
    let mut config = HighlightConfiguration::new(
        language,
        name,
        highlights_query,
        injections_query,
        locals_query,
    )
    .ok()?;
    config.configure(HIGHLIGHT_TAG_NAMES);
    Some(config)
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 Rust 高亮配置。
fn build_rust_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_rust::LANGUAGE.into(),
        "rust",
        tree_sitter_rust::HIGHLIGHTS_QUERY,
        tree_sitter_rust::INJECTIONS_QUERY,
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 JavaScript 高亮配置。
fn build_javascript_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_javascript::LANGUAGE.into(),
        "javascript",
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_javascript::INJECTIONS_QUERY,
        tree_sitter_javascript::LOCALS_QUERY,
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 JSX 高亮配置。
fn build_jsx_config() -> Option<HighlightConfiguration> {
    let query = format!(
        "{}\n{}",
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
    );
    configure_highlights(
        tree_sitter_javascript::LANGUAGE.into(),
        "javascript",
        &query,
        tree_sitter_javascript::INJECTIONS_QUERY,
        tree_sitter_javascript::LOCALS_QUERY,
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 TypeScript 高亮配置。
fn build_typescript_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "typescript",
        tree_sitter_typescript::HIGHLIGHTS_QUERY,
        "",
        tree_sitter_typescript::LOCALS_QUERY,
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 TSX 高亮配置。
fn build_tsx_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_typescript::LANGUAGE_TSX.into(),
        "tsx",
        tree_sitter_typescript::HIGHLIGHTS_QUERY,
        "",
        tree_sitter_typescript::LOCALS_QUERY,
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 JSON 高亮配置。
fn build_json_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_json::LANGUAGE.into(),
        "json",
        tree_sitter_json::HIGHLIGHTS_QUERY,
        "",
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 Markdown 高亮配置。
fn build_markdown_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_md::LANGUAGE.into(),
        "markdown",
        tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
        tree_sitter_md::INJECTION_QUERY_BLOCK,
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 Bash 高亮配置。
fn build_bash_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_bash::LANGUAGE.into(),
        "bash",
        tree_sitter_bash::HIGHLIGHT_QUERY,
        "",
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 C 高亮配置。
fn build_c_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_c::LANGUAGE.into(),
        "c",
        tree_sitter_c::HIGHLIGHT_QUERY,
        "",
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 C++ 高亮配置。
fn build_cpp_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_cpp::LANGUAGE.into(),
        "cpp",
        tree_sitter_cpp::HIGHLIGHT_QUERY,
        "",
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 C# 高亮配置。
fn build_csharp_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_c_sharp::LANGUAGE.into(),
        "c_sharp",
        tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
        "",
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 CSS 高亮配置。
fn build_css_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_css::LANGUAGE.into(),
        "css",
        tree_sitter_css::HIGHLIGHTS_QUERY,
        "",
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 Go 高亮配置。
fn build_go_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_go::LANGUAGE.into(),
        "go",
        tree_sitter_go::HIGHLIGHTS_QUERY,
        "",
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 HTML 高亮配置。
fn build_html_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_html::LANGUAGE.into(),
        "html",
        tree_sitter_html::HIGHLIGHTS_QUERY,
        tree_sitter_html::INJECTIONS_QUERY,
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 Java 高亮配置。
fn build_java_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_java::LANGUAGE.into(),
        "java",
        tree_sitter_java::HIGHLIGHTS_QUERY,
        "",
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 PHP 高亮配置。
fn build_php_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_php::LANGUAGE_PHP.into(),
        "php",
        tree_sitter_php::HIGHLIGHTS_QUERY,
        tree_sitter_php::INJECTIONS_QUERY,
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 Python 高亮配置。
fn build_python_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_python::LANGUAGE.into(),
        "python",
        tree_sitter_python::HIGHLIGHTS_QUERY,
        "",
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 Ruby 高亮配置。
fn build_ruby_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_ruby::LANGUAGE.into(),
        "ruby",
        tree_sitter_ruby::HIGHLIGHTS_QUERY,
        "",
        tree_sitter_ruby::LOCALS_QUERY,
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
/// 构建 YAML 高亮配置。
fn build_yaml_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_yaml::LANGUAGE.into(),
        "yaml",
        tree_sitter_yaml::HIGHLIGHTS_QUERY,
        "",
        "",
    )
}

#[cfg(all(feature = "code-highlight-core", feature = "code-highlight-config"))]
/// 构建 TOML 高亮配置。
fn build_toml_config() -> Option<HighlightConfiguration> {
    configure_highlights(
        tree_sitter_toml::LANGUAGE.into(),
        "toml",
        tree_sitter_toml::HIGHLIGHTS_QUERY,
        "",
        "",
    )
}

/// 按别名查找语言描述符。
fn descriptor_for_language(language: &str) -> Option<&'static HighlightLanguageSpec> {
    HIGHLIGHT_LANGUAGES.iter().find(|descriptor| {
        descriptor
            .aliases
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case(language))
    })
}

/// 解析围栏信息串为语言键。
pub(crate) fn resolve_code_language_key(language: Option<&str>) -> Option<HighlightLanguage> {
    let normalized = language?
        .split_whitespace()
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;

    descriptor_for_language(normalized).map(|descriptor| descriptor.key)
}

/// 高亮代码块源码。
// 无 tree-sitter 核心时只解析语言键，源码不参与高亮，参数留给核心分支使用。
#[cfg_attr(not(feature = "code-highlight-core"), allow(unused_variables))]
pub(crate) fn highlight_code_block(
    language: Option<&str>,
    source: &str,
) -> Option<HighlightResult> {
    let key = resolve_code_language_key(language)?;

    #[cfg(feature = "code-highlight-core")]
    if let Some(config) = CODE_HIGHLIGHT_REGISTRY.config_for(key) {
        let mut highlighter = Highlighter::new();
        let events = match highlighter.highlight(config, source.as_bytes(), None, |_| None) {
            Ok(events) => events,
            Err(_) => {
                return Some(HighlightResult {
                    language: key,
                    spans: Vec::new(),
                });
            }
        };

        let mut spans = Vec::new();
        let mut active = Vec::new();
        for event in events {
            let Ok(event) = event else {
                return Some(HighlightResult {
                    language: key,
                    spans: Vec::new(),
                });
            };

            match event {
                HighlightEvent::Source { start, end } => {
                    if let Some(class) = active.last().copied() {
                        push_highlight_span(&mut spans, start..end, class);
                    }
                }
                HighlightEvent::HighlightStart(highlight) => {
                    if let Some(class) = class_for_highlight(highlight) {
                        active.push(class);
                    }
                }
                HighlightEvent::HighlightEnd => {
                    active.pop();
                }
            }
        }

        return Some(HighlightResult {
            language: key,
            spans,
        });
    }

    Some(HighlightResult {
        language: key,
        spans: Vec::new(),
    })
}

/// 追加高亮区间并合并相邻同类区间。
#[cfg(feature = "code-highlight-core")]
fn push_highlight_span(spans: &mut Vec<HighlightSpan>, range: Range<usize>, class: HighlightClass) {
    if range.start >= range.end {
        return;
    }

    if let Some(last) = spans.last_mut()
        && last.class == class
        && last.range.end == range.start
    {
        last.range.end = range.end;
        return;
    }

    spans.push(HighlightSpan { range, class });
}

/// 把高亮索引映射为语义类别。
#[cfg(feature = "code-highlight-core")]
fn class_for_highlight(highlight: Highlight) -> Option<HighlightClass> {
    let name = HIGHLIGHT_TAG_NAMES.get(highlight.0)?;
    Some(match *name {
        "comment" => HighlightClass::Comment,
        "keyword" | "tag" => HighlightClass::Keyword,
        "string" | "string.special" | "embedded" => HighlightClass::String,
        "number" => HighlightClass::Number,
        "type" | "type.builtin" | "module" => HighlightClass::Type,
        "function" | "function.builtin" | "constructor" => HighlightClass::Function,
        "constant" | "constant.builtin" => HighlightClass::Constant,
        "variable" | "variable.builtin" | "variable.parameter" => HighlightClass::Variable,
        "property" | "property.builtin" | "attribute" => HighlightClass::Property,
        "operator" => HighlightClass::Operator,
        "punctuation" | "punctuation.bracket" | "punctuation.delimiter" | "punctuation.special" => {
            HighlightClass::Punctuation
        }
        _ => return None,
    })
}

/// 取高亮类别对应的组件调色板颜色。
pub(crate) fn code_highlight_color(theme: &rgpui::Theme, class: HighlightClass) -> Hsla {
    match class {
        HighlightClass::Comment => theme.muted_foreground,
        HighlightClass::Keyword => theme.magenta,
        HighlightClass::String => theme.green,
        HighlightClass::Number => theme.yellow,
        HighlightClass::Type => theme.cyan,
        HighlightClass::Function => theme.blue,
        HighlightClass::Constant => theme.yellow_light,
        HighlightClass::Variable => theme.foreground,
        HighlightClass::Property => theme.cyan_light,
        HighlightClass::Operator => theme.muted_foreground,
        HighlightClass::Punctuation => theme.muted_foreground,
    }
}

#[cfg(test)]
mod tests {
    use super::{HighlightLanguage, highlight_code_block, resolve_code_language_key};

    #[test]
    fn balanced_bundle_aliases_resolve_to_expected_keys() {
        assert_eq!(
            resolve_code_language_key(Some("rust")),
            Some(HighlightLanguage::Rust)
        );
        assert_eq!(
            resolve_code_language_key(Some("js")),
            Some(HighlightLanguage::JavaScript)
        );
        assert_eq!(
            resolve_code_language_key(Some("jsx")),
            Some(HighlightLanguage::JavaScriptJsx)
        );
        assert_eq!(
            resolve_code_language_key(Some("ts")),
            Some(HighlightLanguage::TypeScript)
        );
        assert_eq!(
            resolve_code_language_key(Some("tsx")),
            Some(HighlightLanguage::TypeScriptTsx)
        );
        assert_eq!(
            resolve_code_language_key(Some("sh")),
            Some(HighlightLanguage::Bash)
        );
        assert_eq!(
            resolve_code_language_key(Some("hpp")),
            Some(HighlightLanguage::Cpp)
        );
        assert_eq!(
            resolve_code_language_key(Some("c#")),
            Some(HighlightLanguage::CSharp)
        );
        assert_eq!(
            resolve_code_language_key(Some("golang")),
            Some(HighlightLanguage::Go)
        );
        assert_eq!(
            resolve_code_language_key(Some("py")),
            Some(HighlightLanguage::Python)
        );
        assert_eq!(
            resolve_code_language_key(Some("rb")),
            Some(HighlightLanguage::Ruby)
        );
        assert_eq!(
            resolve_code_language_key(Some("yml")),
            Some(HighlightLanguage::Yaml)
        );
        assert_eq!(
            resolve_code_language_key(Some("plain")),
            Some(HighlightLanguage::PlainText)
        );
        assert_eq!(
            resolve_code_language_key(Some("mermaid")),
            Some(HighlightLanguage::Mermaid)
        );
        assert_eq!(resolve_code_language_key(Some("unknown")), None);
    }

    #[test]
    fn plain_fallback_languages_produce_empty_spans() {
        let mermaid = highlight_code_block(Some("mermaid"), "graph TD;\nA-->B")
            .expect("known plain fallback should still produce a result");
        assert_eq!(mermaid.language, HighlightLanguage::Mermaid);
        assert!(mermaid.spans.is_empty());

        let text = highlight_code_block(Some("text"), "just text")
            .expect("plain text should still produce a result");
        assert_eq!(text.language, HighlightLanguage::PlainText);
        assert!(text.spans.is_empty());
    }

    #[cfg(all(feature = "code-highlight-core", feature = "code-highlight-official"))]
    #[test]
    fn default_official_highlight_bundle_produces_spans() {
        let samples = [
            ("rust", "fn main() {\n    let value: i32 = 42;\n}\n"),
            ("js", "function greet(name) { return `hi ${name}`; }\n"),
            ("jsx", "const App = () => <div className=\"x\">Hi</div>;\n"),
            (
                "ts",
                "type User = { id: number };\nconst user: User = { id: 1 };\n",
            ),
            (
                "tsx",
                "const App = (): JSX.Element => <button>OK</button>;\n",
            ),
            ("json", "{\n  \"answer\": 42\n}\n"),
            ("md", "# Heading\n\n`code`\n"),
            ("bash", "echo \"hello\"\nif [ -f file ]; then echo ok; fi\n"),
            ("c", "int main(void) { return 0; }\n"),
            ("cpp", "class Box { public: int value = 1; };\n"),
            (
                "csharp",
                "class App { static void Main() { var x = 1; } }\n",
            ),
            ("css", "body { color: #fff; display: grid; }\n"),
            ("go", "package main\nfunc main() { println(\"hi\") }\n"),
            ("html", "<div class=\"card\"><span>Hi</span></div>\n"),
            (
                "java",
                "class App { int add(int a, int b) { return a + b; } }\n",
            ),
            ("php", "<?php echo \"hi\"; $x = 1; ?>\n"),
            ("python", "def double(x):\n    return x * 2\n"),
            ("ruby", "def hello(name)\n  puts \"Hi #{name}\"\nend\n"),
        ];

        for (language, sample) in samples {
            let result = highlight_code_block(Some(language), sample)
                .expect("known language should produce a result");
            assert!(
                !result.spans.is_empty(),
                "expected non-empty spans for {language}"
            );
        }
    }

    #[cfg(all(feature = "code-highlight-core", feature = "code-highlight-config"))]
    #[test]
    fn config_language_bundle_produces_spans() {
        let yaml = highlight_code_block(Some("yaml"), "key:\n  - value\n")
            .expect("yaml should produce a result");
        assert!(!yaml.spans.is_empty());

        let toml = highlight_code_block(
            Some("toml"),
            "[package]\nname = \"rumark\"\nversion = \"0.1.0\"\n",
        )
        .expect("toml should produce a result");
        assert!(!toml.spans.is_empty());
    }
}
