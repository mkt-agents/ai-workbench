//! 企微 DSH 引擎的 profile 镜像。
//!
//! 背景：DSH 的每个 profile 是一套**独立组合**（`package.json` 的
//! `dsh.profile.bundles` + 自己的 `cordis.patch.yml`）。用户真正在用的模型
//! provider（workbuddy / qoder / MiMo / OpenCode Zen …）都来自 `web` profile
//! 里的第三方 bundle，`acp` profile 默认只有 `dsh-base` + `dsh-acp-app`，
//! 所以一进企微就落到 `deepseek-official` 上、报「no API key」。
//!
//! 这里把 `~/.dsh/profiles/acp` 做成 `web` 的镜像：
//! - bundle 列表照抄，`dsh-web-app` 换成 `dsh-acp-app`，去掉插件市场 UI；
//! - `node_modules` 用目录联接（junction）指向 web 的，不重装、不联网；
//! - `cordis.patch.yml` 照抄，只滤掉 `ui-*` 与市场相关条目；
//! - `compatibility.json` 一并镜像。
//!
//! 只写 acp 目录，绝不改 `profiles/web` 与凭证库。

use std::path::{Path, PathBuf};

const SOURCE_PROFILE: &str = "web";
const TARGET_PROFILE: &str = "acp";
const BASE_BUNDLE: &str = "@deepseek-ai/dsh-base";
const WEB_APP_BUNDLE: &str = "@deepseek-ai/dsh-web-app";
const ACP_APP_BUNDLE: &str = "@deepseek-ai/dsh-acp-app";

/// 插件市场 / 纯 UI 的 bundle：ACP 进程里没有浏览器，带了只会白加载。
const DROPPED_BUNDLES: &[&str] = &["@dsh-market/plugin", "dshmarket"];

const PNPM_WORKSPACE: &str = "packages:
  - .

nodeLinker: hoisted
autoInstallPeers: false
";

const ROOT_CONFIG: &str = "# dsh profile root — an empty entry list. The tree is composed as patches:
# each bundle in package.json's dsh.profile.bundles, then cordis.patch.yml, then any
# --patch overlays. Edit cordis.patch.yml, not this file.
[]
";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorReport {
    /// 镜像来源 profile 名（`web`）
    pub source: String,
    /// 目标 profile 名（`acp`）
    pub target: String,
    /// 镜像后的 bundle 数
    pub bundles: usize,
    /// 被丢弃的 bundle（市场 / UI）
    pub dropped: Vec<String>,
    /// 带过来的 patch 条目数
    pub patch_entries: usize,
    /// `node_modules` 是否已经指向来源 profile
    pub linked: bool,
    /// 同步完成时间（毫秒时间戳）
    pub synced_at: u64,
    /// 没做到的部分（例如 web profile 不存在、目标已有 node_modules）
    pub warning: Option<String>,
}

impl MirrorReport {
    fn skipped(reason: &str) -> Self {
        Self {
            source: SOURCE_PROFILE.into(),
            target: TARGET_PROFILE.into(),
            bundles: 0,
            dropped: Vec::new(),
            patch_entries: 0,
            linked: false,
            synced_at: now_ms(),
            warning: Some(reason.to_string()),
        }
    }
}

/// 解析 DSH home（`~/.dsh`）。
pub fn dsh_profiles_root() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())?;
    Some(home.join(".dsh").join("profiles"))
}

/// 镜像一次。来源不存在时返回带 warning 的报告（按 DSH 默认 acp 组合启动），
/// 写盘 / 联接失败才返回 `Err`——那是真的起不来，不能静默。
pub fn mirror_profiles(profiles_root: &Path) -> Result<MirrorReport, String> {
    let source = profiles_root.join(SOURCE_PROFILE);
    let target = profiles_root.join(TARGET_PROFILE);
    mirror(&source, &target)
}

/// 可单测的实现：来源、目标都显式传入。
pub fn mirror(source: &Path, target: &Path) -> Result<MirrorReport, String> {
    let manifest_path = source.join("package.json");
    if !manifest_path.is_file() {
        return Ok(MirrorReport::skipped(
            "没有找到 DSH 的 web profile：按 DSH 默认 acp 组合启动（模型可能为空）",
        ));
    }

    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("读取 {} 失败：{e}", manifest_path.display()))?,
    )
    .map_err(|e| format!("解析 {} 失败：{e}", manifest_path.display()))?;

    let (bundles, dropped) = mirror_bundles(&manifest);
    std::fs::create_dir_all(target)
        .map_err(|e| format!("创建 {} 失败：{e}", target.display()))?;

    // 1) manifest：依赖照抄（解析第三方 bundle 靠它），bundle 列表换成镜像后的。
    let mut out = serde_json::Map::new();
    out.insert(
        "name".into(),
        serde_json::Value::String(format!("dsh-profile-{TARGET_PROFILE}")),
    );
    out.insert("private".into(), serde_json::Value::Bool(true));
    out.insert(
        "dependencies".into(),
        manifest
            .get("dependencies")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    );
    let mut profile = serde_json::Map::new();
    profile.insert(
        "bundles".into(),
        serde_json::Value::Array(
            bundles
                .iter()
                .map(|b| serde_json::Value::String(b.clone()))
                .collect(),
        ),
    );
    if let Some(reload) = manifest
        .get("dsh")
        .and_then(|d| d.get("profile"))
        .and_then(|p| p.get("patchReload"))
    {
        profile.insert("patchReload".into(), reload.clone());
    }
    out.insert("dsh".into(), serde_json::json!({ "profile": profile }));
    write_json(&target.join("package.json"), &serde_json::Value::Object(out))?;

    // 2) 用户 patch 层：过滤后整段带过来（保留原格式，`!!js` 不会被重新序列化）。
    let patch_src = source.join("cordis.patch.yml");
    let (patch_text, patch_entries) = if patch_src.is_file() {
        let raw = std::fs::read_to_string(&patch_src)
            .map_err(|e| format!("读取 {} 失败：{e}", patch_src.display()))?;
        let (header, kept, count) = filter_patch(&raw);
        let mut text = String::new();
        text.push_str("# ai-workbench 生成：镜像 DSH 的 web profile（模型 / provider 配置）。\n");
        text.push_str("# 每次启动重写；DSH 里改了插件或模型后请点设置页的「重新同步」。\n");
        if !header.is_empty() {
            text.push_str(&header);
        }
        text.push_str(&kept);
        (text, count)
    } else {
        ("# ai-workbench 生成：来源 profile 没有用户 patch 层。\n[]\n".to_string(), 0)
    };
    std::fs::write(target.join("cordis.patch.yml"), patch_text)
        .map_err(|e| format!("写入 acp cordis.patch.yml 失败：{e}"))?;

    // 3) 其余模板文件：loader 需要一个真实的 include 锚点与 pnpm 设置。
    if !target.join("cordis.yml").is_file() {
        std::fs::write(target.join("cordis.yml"), ROOT_CONFIG)
            .map_err(|e| format!("写入 acp cordis.yml 失败：{e}"))?;
    }
    if !target.join("pnpm-workspace.yaml").is_file() {
        std::fs::write(target.join("pnpm-workspace.yaml"), PNPM_WORKSPACE)
            .map_err(|e| format!("写入 acp pnpm-workspace.yaml 失败：{e}"))?;
    }

    // 4) 版本豁免表：第三方插件常用它绕过 `dsh` 兼容性检查。
    let compat_src = source.join("compatibility.json");
    if compat_src.is_file() {
        let _ = std::fs::copy(&compat_src, target.join("compatibility.json"));
    }

    // 5) node_modules 联接：让第三方 bundle 能从 acp 目录解析到。
    let (linked, link_warning) = link_node_modules(source, target)?;

    Ok(MirrorReport {
        source: SOURCE_PROFILE.into(),
        target: TARGET_PROFILE.into(),
        bundles: bundles.len(),
        dropped,
        patch_entries,
        linked,
        synced_at: now_ms(),
        warning: link_warning,
    })
}

/// bundle 列表映射：web-app → acp-app，丢掉市场 UI，其余保持原顺序。
pub fn mirror_bundles(manifest: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let mut out: Vec<String> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    let mut has_app = false;
    let source = manifest
        .get("dsh")
        .and_then(|d| d.get("profile"))
        .and_then(|p| p.get("bundles"))
        .and_then(|b| b.as_array())
        .cloned()
        .unwrap_or_default();

    for value in source {
        let Some(name) = value.as_str() else { continue };
        if DROPPED_BUNDLES.contains(&name) {
            dropped.push(name.to_string());
            continue;
        }
        if name == WEB_APP_BUNDLE {
            out.push(ACP_APP_BUNDLE.to_string());
            has_app = true;
            continue;
        }
        if name == ACP_APP_BUNDLE {
            has_app = true;
        }
        out.push(name.to_string());
    }
    if !out.iter().any(|b| b == BASE_BUNDLE) {
        out.insert(0, BASE_BUNDLE.to_string());
    }
    if !has_app {
        // 没有 web-app 可替换时，把 acp-app 挂在 base 后面。
        let at = out
            .iter()
            .position(|b| b == BASE_BUNDLE)
            .map(|i| i + 1)
            .unwrap_or(out.len());
        out.insert(at, ACP_APP_BUNDLE.to_string());
    }
    (out, dropped)
}

/// 过滤 profile 的 patch 文本：按顶层条目切块，丢掉 `ui-*` 与市场相关条目。
/// 返回 (头部注释, 保留正文, 保留条目数)。
///
/// 不用 YAML 反序列化：DSH 的 patch 里有 `!!js` 这类自定义标签，重新序列化
/// 有把标签写坏的风险；条目是顶层数组，按行切块既简单又保真。
pub fn filter_patch(raw: &str) -> (String, String, usize) {
    let mut header = String::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Option<String> = None;

    for line in raw.lines() {
        let is_entry_start = line.starts_with("- ") || line.trim_end() == "-";
        if is_entry_start {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            current = Some(format!("{line}\n"));
            continue;
        }
        match current {
            Some(ref mut block) => {
                block.push_str(line);
                block.push('\n');
            }
            None => {
                header.push_str(line);
                header.push('\n');
            }
        }
    }
    if let Some(block) = current {
        blocks.push(block);
    }

    let mut kept = String::new();
    let mut count = 0;
    for block in blocks.into_iter().filter(|b| keep_entry(b)) {
        kept.push_str(&block);
        count += 1;
    }
    if count == 0 && kept.is_empty() {
        kept.push_str("[]\n");
    }
    (header, kept, count)
}

fn keep_entry(block: &str) -> bool {
    let mut id = "";
    let mut name = "";
    for line in block.lines() {
        // 只看条目这一层的键：`- id:`（缩进 0）或 `  id:`（缩进 2），
        // 再深的 `id:` 属于插件配置，不能拿来判断整条要不要丢。
        let indent = line.len() - line.trim_start().len();
        if indent > 2 {
            continue;
        }
        let trimmed = line.trim_start().trim_start_matches("- ").trim();
        if let Some(rest) = trimmed.strip_prefix("id:") {
            id = rest.trim().trim_matches(['"', '\'']);
        } else if let Some(rest) = trimmed.strip_prefix("name:") {
            name = rest.trim().trim_matches(['"', '\'']);
        }
        if !id.is_empty() && !name.is_empty() {
            break;
        }
    }
    if id.starts_with("ui-") {
        return false;
    }
    !id.contains("market") && !name.contains("market")
}

fn link_node_modules(source: &Path, target: &Path) -> Result<(bool, Option<String>), String> {
    let link = target.join("node_modules");
    if link.exists() {
        // 上一次镜像建的联接也在，直接复用；真的目录不删（可能是用户自己装的）。
        return Ok((true, None));
    }
    let modules = source.join("node_modules");
    if !modules.is_dir() {
        return Ok((
            false,
            Some("来源 profile 没有 node_modules，第三方插件可能加载失败".into()),
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("cmd");
        cmd.args([
            "/c",
            "mklink",
            "/J",
            &link.to_string_lossy(),
            &modules.to_string_lossy(),
        ]);
        cmd.creation_flags(0x0800_0000);
        let out = cmd
            .output()
            .map_err(|e| format!("创建 node_modules 目录联接失败：{e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(format!(
                "创建 node_modules 目录联接失败：{}",
                if err.is_empty() {
                    format!("mklink 退出码 {:?}", out.status.code())
                } else {
                    err
                }
            ));
        }
        Ok((true, None))
    }
    #[cfg(not(windows))]
    {
        let _ = (source, target);
        Err("企微 DSH 引擎仅支持 Windows".into())
    }
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, format!("{text}\n"))
        .map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aw-wecom-mirror-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn bundles_drop_market_and_swap_app() {
        let manifest = serde_json::json!({
            "dsh": { "profile": { "bundles": [
                "@deepseek-ai/dsh-base",
                "@deepseek-ai/dsh-web-app",
                "dsh-provider-qoder",
                "@dsh-market/plugin",
                "dshmarket",
                "dsh-connect-workbuddy"
            ] } }
        });
        let (bundles, dropped) = mirror_bundles(&manifest);
        assert_eq!(
            bundles,
            vec![
                "@deepseek-ai/dsh-base",
                "@deepseek-ai/dsh-acp-app",
                "dsh-provider-qoder",
                "dsh-connect-workbuddy"
            ]
        );
        assert_eq!(dropped, vec!["@dsh-market/plugin", "dshmarket"]);
    }

    #[test]
    fn bundles_without_web_app_get_acp_app_after_base() {
        let manifest = serde_json::json!({
            "dsh": { "profile": { "bundles": ["@deepseek-ai/dsh-base", "x"] } }
        });
        let (bundles, _) = mirror_bundles(&manifest);
        assert_eq!(bundles, vec!["@deepseek-ai/dsh-base", ACP_APP_BUNDLE, "x"]);
    }

    #[test]
    fn patch_filter_drops_ui_and_keeps_js_tags() {
        let raw = "# header\n- id: ui-theme\n  config:\n    preference: dark\n- id: llm-pi-ai\n  config:\n    providers:\n      a:\n        apiKeyEnv: A_KEY\n- id: agent-default-model\n  config:\n    provider: workbuddy\n    model: deepseek-v4.1-flash\n    disabled: !!js 'process.platform === \\'win32\\''\n";
        let (header, kept, count) = filter_patch(raw);
        assert!(header.contains("# header"));
        assert!(!kept.contains("ui-theme"));
        assert!(kept.contains("llm-pi-ai"));
        assert!(kept.contains("!!js"), "自定义标签必须原样保留：{kept}");
        assert_eq!(count, 2);
    }

    #[test]
    fn patch_filter_drops_market_entries() {
        let raw = "- id: dshmarket\n  config: {}\n- insert:\n    - id: dsh-purge\n";
        let (_, kept, count) = filter_patch(raw);
        assert!(!kept.contains("dshmarket"));
        assert!(kept.contains("dsh-purge"));
        assert_eq!(count, 1);
    }

    #[test]
    fn mirror_writes_manifest_and_patch() {
        let source = temp_dir("src");
        let target = temp_dir("dst");
        std::fs::write(
            source.join("package.json"),
            serde_json::json!({
                "dependencies": { "dsh-connect-workbuddy": "5.12.1" },
                "dsh": { "profile": { "bundles": [
                    "@deepseek-ai/dsh-base",
                    "@deepseek-ai/dsh-web-app",
                    "dsh-connect-workbuddy"
                ], "patchReload": "live" } }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            source.join("cordis.patch.yml"),
            "- id: agent-default-model\n  config:\n    provider: workbuddy\n- id: ui-theme\n  config: {}\n",
        )
        .unwrap();
        std::fs::write(source.join("compatibility.json"), "{}").unwrap();

        let report = mirror(&source, &target).unwrap();
        assert_eq!(report.bundles, 3);
        assert_eq!(report.patch_entries, 1);
        assert!(report.warning.is_some(), "来源没有 node_modules 时要给出提示");

        let manifest = std::fs::read_to_string(target.join("package.json")).unwrap();
        assert!(manifest.contains("dsh-acp-app"));
        assert!(!manifest.contains("dsh-web-app"));
        assert!(manifest.contains("patchReload"));

        let patch = std::fs::read_to_string(target.join("cordis.patch.yml")).unwrap();
        assert!(patch.contains("workbuddy"));
        assert!(!patch.contains("ui-theme"));
        assert!(target.join("compatibility.json").is_file());
        assert!(target.join("pnpm-workspace.yaml").is_file());

        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(&target);
    }

    /// 对真实 DSH home 跑一次镜像（只写 `~/.dsh/profiles/acp`），联调时手动执行：
    /// `cargo test --lib mirror_real_profile -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn mirror_real_profile() {
        let root = dsh_profiles_root().expect("USERPROFILE 应存在");
        let report = mirror_profiles(&root).expect("镜像失败");
        println!("{report:#?}");
        assert!(report.bundles > 0, "镜像应至少保留 base bundle");
    }

    #[test]
    fn missing_source_is_not_fatal() {
        let root = temp_dir("none");
        let report = mirror(&root.join("web"), &root.join("acp")).unwrap();
        assert_eq!(report.bundles, 0);
        assert!(report.warning.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }
}
