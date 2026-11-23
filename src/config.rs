//! 配置加载：全局 `<config_dir>/ferro/config.toml` + 项目级 `.ferro/config.toml`（后者覆盖前者）

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::ai::{anthropic::AnthropicProvider, openai::OpenAiProvider, Provider};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Openai,
    Anthropic,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "type")]
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    /// 直接写 key，或 `env:VAR_NAME` 从环境变量读取
    pub api_key: Option<String>,
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub thinking_budget: u32,
    pub reasoning_passback: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PermissionConfig {
    /// edit / write / multi_edit 的默认动作（与 OpenCode 一致，默认 allow）
    #[serde(default = "default_allow")]
    pub edit: Action,
    /// bash 命令规则：glob 模式 -> 动作，`*` 为兜底
    #[serde(default = "default_bash_rules")]
    pub bash: BTreeMap<String, Action>,
}

fn default_allow() -> Action {
    Action::Allow
}

fn default_bash_rules() -> BTreeMap<String, Action> {
    // 与 OpenCode 默认行为一致：bash 默认放行；危险命令单独拦截
    let mut m = BTreeMap::new();
    m.insert("*".into(), Action::Allow);
    for p in ["rm -rf *", "rm -fr *", "git push --force*", "git reset --hard*", "sudo *", "mkfs*", "dd *", "Remove-Item * -Recurse*", "format *"] {
        m.insert(p.into(), Action::Ask);
    }
    m
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self { edit: Action::Allow, bash: default_bash_rules() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    /// `provider_id/model_name`
    pub model: Option<String>,
    pub shell: Option<String>,
    pub max_steps: Option<usize>,
    #[serde(default)]
    pub provider: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub permission: PermissionConfig,
    #[serde(skip)]
    pub discovered_models: Vec<ModelOption>,
}

pub fn global_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("ferro")
}

impl Config {
    pub fn load(cwd: &Path) -> Result<Self> {
        let mut merged = toml::Value::Table(Default::default());
        for path in [global_dir().join("config.toml"), cwd.join(".ferro").join("config.toml")] {
            if path.exists() {
                let text = std::fs::read_to_string(&path)?;
                let v: toml::Value =
                    toml::from_str(&text).with_context(|| format!("配置文件格式错误: {}", path.display()))?;
                merge(&mut merged, v);
            }
        }
        let mut cfg: Config = merged.try_into().context("配置解析失败")?;
        cfg.import_local_magpie_and_opencode();
        cfg.add_builtin_providers();
        // 后台异步检测/拉起本地 Magpie，不阻塞客户端窗口冷启动
        let cfg_bg = cfg.clone();
        std::thread::spawn(move || {
            cfg_bg.ensure_local_magpie_running();
        });
        Ok(cfg)
    }

    /// 自动扫描本机已有的 OpenCode / Magpie 本地代理网关配置与模型列表
    fn import_local_magpie_and_opencode(&mut self) {
        let Some(home) = dirs::home_dir() else { return };
        let magpie_dir = home.join(".config").join("magpie");

        // 1. 读取 ~/.config/magpie/gateway.json 中的本地代理端口（默认 127.0.0.1:3425）
        let mut magpie_base = "http://127.0.0.1:3425/v1".to_string();
        if let Ok(text) = std::fs::read_to_string(magpie_dir.join("gateway.json")) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if let Some(addr) = v["addr"].as_str().filter(|s| !s.is_empty()) {
                    magpie_base = format!("http://{}/v1", addr.trim_end_matches('/'));
                }
            }
        }

        // 2. 读取 ~/.config/magpie/caller-keys.json 作为备用本地密钥
        let mut magpie_fallback_key: Option<String> = None;
        if let Ok(text) = std::fs::read_to_string(magpie_dir.join("caller-keys.json")) {
            if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&text) {
                magpie_fallback_key = arr
                    .first()
                    .and_then(|item| item["secret"].as_str())
                    .map(String::from);
            }
        }

        // 3. 读取 ~/.config/opencode/opencode.json（Magpie 会把所有已登录的 Codex / Antigravity 模型同步在此）
        let opencode_paths = [
            home.join(".config").join("opencode").join("opencode.json"),
            dirs::config_dir()
                .unwrap_or_else(|| home.clone())
                .join("opencode")
                .join("opencode.json"),
        ];
        for oc_path in opencode_paths {
            let Ok(text) = std::fs::read_to_string(&oc_path) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if self.model.is_none() {
                if let Some(m) = v["model"].as_str().filter(|s| !s.is_empty()) {
                    self.model = Some(m.to_string());
                }
            }
            if let Some(provs) = v["provider"].as_object() {
                for (pid, pval) in provs {
                    let npm = pval["npm"].as_str().unwrap_or("");
                    let kind = if npm.contains("anthropic") {
                        ProviderKind::Anthropic
                    } else {
                        ProviderKind::Openai
                    };
                    let base_url = pval["options"]["baseURL"]
                        .as_str()
                        .or_else(|| pval["options"]["baseUrl"].as_str())
                        .map(String::from)
                        .or_else(|| {
                            if pid == "magpie" {
                                Some(magpie_base.clone())
                            } else {
                                None
                            }
                        });
                    let api_key = pval["options"]["apiKey"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .or_else(|| {
                            if pid == "magpie" {
                                magpie_fallback_key.clone().or_else(|| Some("magpie".into()))
                            } else {
                                None
                            }
                        });

                    if base_url.is_some() {
                        self.provider.entry(pid.clone()).or_insert(ProviderConfig {
                            kind,
                            base_url,
                            api_key,
                            max_tokens: None,
                            thinking_budget: 0,
                            reasoning_passback: None,
                        });
                    }

                    if let Some(models) = pval["models"].as_object() {
                        for (mid, mval) in models {
                            let full_id = format!("{}/{}", pid, mid);
                            if !self.discovered_models.iter().any(|x| x.id == full_id) {
                                let raw_name = mval["name"].as_str().unwrap_or(mid);
                                // 修复部分 GBK/UTF-8 编码里 "·" 显示为 "路" 的情况
                                let label = raw_name.replace(" 路 ", " · ");
                                self.discovered_models.push(ModelOption { id: full_id, label });
                            }
                        }
                    }
                }
            }
            break;
        }
    }

    /// 若当前配置了本地 Magpie 网关但端口未监听，自动在后台拉起本机 Magpie 进程
    pub fn ensure_local_magpie_running(&self) {
        let Some(pc) = self.provider.get("magpie") else {
            return;
        };
        let Some(url) = pc.base_url.as_deref() else {
            return;
        };
        let host_port = url
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .split('/')
            .next()
            .unwrap_or("127.0.0.1:3425");
        if let Ok(addr) = host_port.parse::<SocketAddr>() {
            if TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_ok() {
                return; // 已在运行
            }
        }
        #[cfg(windows)]
        {
            let mut candidates: Vec<PathBuf> = Vec::new();
            if let Some(home) = dirs::home_dir() {
                candidates.push(home.join("Downloads").join("magpie-windows-amd64.exe"));
                candidates.push(home.join("AppData").join("Local").join("magpie").join("magpie.exe"));
            }
            for exe in candidates {
                if exe.is_file() {
                    let _ = std::process::Command::new(&exe).spawn();
                    return;
                }
            }
            if let Some(appdata) = dirs::config_dir() {
                let lnk = appdata
                    .join("Microsoft")
                    .join("Windows")
                    .join("Start Menu")
                    .join("Programs")
                    .join("Magpie.lnk");
                if lnk.is_file() {
                    use std::os::windows::process::CommandExt;
                    let _ = std::process::Command::new("cmd.exe")
                        .creation_flags(0x0800_0000)
                        .args(["/C", "start", "", &lnk.to_string_lossy()])
                        .spawn();
                }
            }
        }
    }

    /// 内置常用服务商预设，用户配置同名项时以用户为准
    fn add_builtin_providers(&mut self) {
        let presets: [(&str, ProviderKind, &str, Option<&str>); 8] = [
            ("anthropic", ProviderKind::Anthropic, "https://api.anthropic.com/v1", Some("env:ANTHROPIC_API_KEY")),
            ("openai", ProviderKind::Openai, "https://api.openai.com/v1", Some("env:OPENAI_API_KEY")),
            ("deepseek", ProviderKind::Openai, "https://api.deepseek.com/v1", Some("env:DEEPSEEK_API_KEY")),
            ("qwen", ProviderKind::Openai, "https://dashscope.aliyuncs.com/compatible-mode/v1", Some("env:DASHSCOPE_API_KEY")),
            ("moonshot", ProviderKind::Openai, "https://api.moonshot.cn/v1", Some("env:MOONSHOT_API_KEY")),
            ("zhipu", ProviderKind::Openai, "https://open.bigmodel.cn/api/paas/v4", Some("env:ZHIPU_API_KEY")),
            ("openrouter", ProviderKind::Openai, "https://openrouter.ai/api/v1", Some("env:OPENROUTER_API_KEY")),
            ("ollama", ProviderKind::Openai, "http://localhost:11434/v1", None),
        ];
        for (id, kind, url, key) in presets {
            self.provider.entry(id.to_string()).or_insert(ProviderConfig {
                kind,
                base_url: Some(url.into()),
                api_key: key.map(String::from),
                max_tokens: None,
                thinking_budget: 0,
                reasoning_passback: None,
            });
        }
    }

    /// 未指定模型时，根据已设置的环境变量自动挑选；若都未设置则默认返回 deepseek/deepseek-chat 以便直接进入界面
    pub fn resolve_model(&self, cli: Option<&str>) -> String {
        if let Some(m) = cli.map(String::from).or_else(|| self.model.clone()) {
            return m;
        }
        let candidates = [
            ("ANTHROPIC_API_KEY", "anthropic/claude-sonnet-4-5"),
            ("DEEPSEEK_API_KEY", "deepseek/deepseek-chat"),
            ("OPENAI_API_KEY", "openai/gpt-4.1"),
            ("DASHSCOPE_API_KEY", "qwen/qwen3-coder-plus"),
            ("MOONSHOT_API_KEY", "moonshot/kimi-k2-turbo-preview"),
            ("OPENROUTER_API_KEY", "openrouter/anthropic/claude-sonnet-4.5"),
        ];
        for (env, model) in candidates {
            if std::env::var(env).map(|v| !v.is_empty()).unwrap_or(false) {
                return model.to_string();
            }
        }
        "deepseek/deepseek-chat".to_string()
    }

    /// 检查当前模型是否已具备可用的 API Key（ollama 等无需 key 的返回 true）
    pub fn has_api_key(&self, model_ref: &str) -> bool {
        let Some((pid, _)) = model_ref.split_once('/') else {
            return false;
        };
        let Some(pc) = self.provider.get(pid) else {
            return false;
        };
        match pc.api_key.as_deref() {
            Some(k) if k.starts_with("env:") => std::env::var(&k[4..]).map(|v| !v.is_empty()).unwrap_or(false),
            Some(k) => !k.is_empty(),
            None => true,
        }
    }

    pub fn set_api_key(&mut self, model_ref: &str, key: &str) -> Result<()> {
        let (pid, _) = model_ref
            .split_once('/')
            .ok_or_else(|| anyhow!("模型格式应为 provider/model"))?;
        let pc = self
            .provider
            .get_mut(pid)
            .ok_or_else(|| anyhow!("未知的 provider: {}", pid))?;
        pc.api_key = Some(key.to_string());
        Ok(())
    }

    pub fn build_provider(&self, model_ref: &str) -> Result<(Arc<dyn Provider>, String)> {
        let (pid, model) = model_ref
            .split_once('/')
            .ok_or_else(|| anyhow!("模型格式应为 provider/model，例如 deepseek/deepseek-chat"))?;
        let pc = self
            .provider
            .get(pid)
            .ok_or_else(|| anyhow!("未知的 provider: {}（可在配置文件 [provider.{}] 中定义）", pid, pid))?;
        let api_key = match pc.api_key.as_deref() {
            Some(k) if k.starts_with("env:") => {
                let var = &k[4..];
                std::env::var(var).ok().filter(|v| !v.is_empty())
            }
            Some(k) => Some(k.to_string()),
            None => None,
        };
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .tcp_nodelay(true)
            .build()?;
        let base_url = pc.base_url.clone().unwrap_or_default();
        let provider: Arc<dyn Provider> = match pc.kind {
            ProviderKind::Openai => Arc::new(OpenAiProvider {
                client,
                reasoning_passback: pc.reasoning_passback.unwrap_or(base_url.contains("deepseek")),
                base_url,
                api_key,
                max_tokens: pc.max_tokens,
            }),
            ProviderKind::Anthropic => Arc::new(AnthropicProvider {
                client,
                base_url,
                api_key,
                max_tokens: pc.max_tokens.unwrap_or(16000),
                thinking_budget: pc.thinking_budget,
            }),
        };
        Ok((provider, model.to_string()))
    }
}

fn merge(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => merge(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

pub const SAMPLE_CONFIG: &str = r#"# Ferro 配置文件
# 模型格式：provider/model
model = "deepseek/deepseek-chat"
# max_steps = 50
# shell = "C:/Program Files/Git/bin/bash.exe"

# 内置 provider：anthropic / openai / deepseek / qwen / moonshot / zhipu / openrouter / ollama
# 默认从对应环境变量读取 key，也可以在这里覆盖或新增任意 OpenAI 兼容服务：
# [provider.myproxy]
# type = "openai"            # openai | anthropic
# base_url = "https://example.com/v1"
# api_key = "env:MY_API_KEY" # 或直接写 key
# max_tokens = 8192

# [provider.anthropic]
# type = "anthropic"
# base_url = "https://api.anthropic.com/v1"
# api_key = "env:ANTHROPIC_API_KEY"
# thinking_budget = 8000     # >0 开启思考

[permission]
edit = "allow"   # allow | ask | deny（与 OpenCode 一致，默认直接修改并展示 diff）

[permission.bash]
"*" = "allow"
"rm -rf *" = "ask"
"git push --force*" = "ask"
"git reset --hard*" = "ask"
"#;
