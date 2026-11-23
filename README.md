<p align="center">
  <img src="assets/ferro-logo.svg" width="84" height="84" alt="Ferro Logo" />
</p>

<h1 align="center">Ferro</h1>

<p align="center">
  轻量、极简、纯原生 Rust 编写的本地 AI 编码桌面客户端（同步提供 TUI 与 CLI）<br/>
  <strong>无需登录 · 不依赖浏览器 · 不监听本地网页端口 · 全简体中文原生界面</strong>
</p>

---

## 核心特性

- **极简无边框原生窗口**：基于 Rust + `egui` (`eframe` / `glow`) 构建，启动即开独立原生窗口，无 Electron/WebView 浏览器子进程，无 `localhost` 网页服务；一体化顶栏支持拖拽移动、双击最大化与四边自由缩放。
- **专注的对话布局**：空会话将标题与输入卡片居中组合在主区中部；开始对话后输入框自动固定到底部并与 `760px` 对话流对齐。支持深浅双主题、窄窗口自动收起侧栏、Markdown 排版、代码块横向滚动与一键复制，工具步骤与 Diff 默认收起。
- **四档精细权限模式**：输入框左下角可随时切换操作审批策略：
  - **请求批准**：编辑文件和执行命令时始终询问
  - **帮我批准**（默认）：常规工作区读写放行，修改外部文件或高危命令先询问确认
  - **完全访问权限**：直接修改文件并执行命令，无需反复确认
  - **只读规划**：仅分析代码与制定方案，禁止修改文件
- **顺手的中文输入体验**：Windows 优先加载微软雅黑，macOS 优先加载苹方；支持**回车发送、`Shift + 回车` 换行**，内置中文输入法（IME）候选上屏防误发保护。
- **自由连接任意模型**：无需 Ferro 账号，不强制依赖 Magpie。内置 **Ollama（本地）** 一键自动发现本机已安装模型，内置 **OpenAI / DeepSeek / Anthropic** 快捷预设与一键拉取接口模型列表，同时也兼容自动导入本机已有 Magpie / OpenCode 配置。
- **多形态复用**：同一份 Rust 核心引擎同时支持原生桌面窗口、终端界面（`--tui`）、纯文本对话（`--no-tui`）和单次非交互命令（`-p`）；支持跨会话全文检索、单轮文件修改快照撤销与 `@路径` 引用文件。

---

## 下载与安装（Windows x64）

当前提供实机验证过的 **Windows x64** 预编译安装包（macOS 暂不提供预编译安装包，请在 Mac 本机按下方步骤从源码编译）。

1. **下载安装包与校验文件**
   前往 [GitHub Releases](https://github.com/hanyuana/ferro/releases) 下载对应版本的：
   - `Ferro-Setup-<version>-x64.exe`（图形安装程序，内含原生 `ferro.exe`、多尺寸图标与卸载器）
   - `Ferro-Setup-<version>-x64.exe.sha256`（SHA-256 校验文件）

2. **校验文件完整性**
   ```powershell
   Get-FileHash .\Ferro-Setup-0.2.0-x64.exe -Algorithm SHA256
   Get-Content .\Ferro-Setup-0.2.0-x64.exe.sha256
   ```

3. **安装、升级与卸载**
   - 双击安装包打开中文安装向导，默认按**当前用户**安装到 `%LOCALAPPDATA%\Programs\Ferro`（无需管理员权限），也可点击「浏览…」自定义目录。
   - 安装后自动创建开始菜单快捷方式 `Ferro` 与 `卸载 Ferro`。
   - **数据安全**：覆盖升级或执行卸载仅替换/移除程序文件与快捷方式，**会完整保留 `%APPDATA%\ferro` 下的历史会话（`sessions/`）与全局配置（`config.toml`）**。

4. **代码签名说明**
   - 当前发布的 Windows 安装包与可执行文件**未附加商业 Authenticode 代码签名证书（Unsigned）**。
   - **请勿关闭 Windows Defender、SmartScreen 或任何系统安全防护**；建议运行前核对官方发布的 SHA-256 校验值，或直接从本仓库源码自行编译。

---

## 快速连接模型（无需 Magpie）

双击启动 `ferro.exe` 后，点击右上角 **`设置 → 模型设置`**（或底部模型下拉菜单中的 **`模型设置…`**）：

### 方案 A：使用本机 Ollama（免费离线）
1. 确保本机已安装并运行 [Ollama](https://ollama.com/)，且至少拉取过一个模型（例如 `ollama pull qwen2.5-coder:7b`）。
2. 在「模型设置」中点击 **`Ollama（本地）`** 快捷按钮。
3. Ferro 会自动请求本机 `http://localhost:11434/api/tags`，列出你已安装的模型供下拉直接点选（无需 API Key）。
4. 勾选「保存到全局配置」，点击 **`应用`** 即可开始对话。

### 方案 B：使用第三方 API 密钥（OpenAI / DeepSeek / Anthropic / 自定义网关）
1. 在「模型设置」中点击 **`OpenAI`**、**`DeepSeek`** 或 **`Anthropic`** 快捷预设（自定义服务商可按 `提供方/模型名` 格式填写，如 `myproxy/model-name`）。
2. 确认「接口地址」（OpenAI 兼容服务通常以 `/v1` 结尾）。
3. 在「接口密钥」中粘贴 `sk-...`，或填写环境变量引用（如 `env:DEEPSEEK_API_KEY`）。
4. 可点击 **`获取接口模型列表`** 自动拉取该接口支持的模型下拉选择。
5. 勾选「保存到全局配置」，点击 **`应用`**。

也可以手动编辑配置文件（全局路径：Windows `%APPDATA%\ferro\config.toml` / macOS `~/Library/Application Support/ferro/config.toml`；项目级覆盖：`<项目根目录>\.ferro\config.toml`）：

```toml
model = "deepseek/deepseek-chat"

[provider.deepseek]
type = "openai"
base_url = "https://api.deepseek.com/v1"
api_key = "env:DEEPSEEK_API_KEY"
```

### 方案 C：已有本地 Magpie 网关
若本机已安装并配置了 Magpie，Ferro 启动时会自动读取本地代理地址与模型列表，底部下拉框直接切换即可。
> 说明：Ferro 内核为 100% 独立纯 Rust 实现；若自动复用了 `~/.config/opencode/opencode.json` 中的 Magpie 调用密钥，Magpie 面板会按该密钥归属显示为 `opencode`。如需单独统计，可在 Magpie 中新建名为 `ferro` 的 Caller Key 并填入 Ferro 的「模型设置」。

---

## 常用快捷键与交互

| 操作 | 快捷键 / 方式 |
| --- | --- |
| 发送消息 | `Enter`（输入法候选上屏时自动防误发） |
| 输入框换行 | `Shift + Enter` |
| 展开 / 收起左侧会话栏 | `Ctrl + B`（窄窗口 `<= 860px` 自动收起） |
| 引用工作区文件 | 输入 `@文件路径` |
| 撤销本轮修改 | 点击左下角模式菜单 → `撤销本轮修改` |
| 切换代码项目 | 点击右上角 `项目` → `打开项目…` 或选择最近项目 |

---

## 命令行与终端模式（CLI / TUI）

```powershell
# 启动原生桌面窗口并指定项目目录
.\ferro.exe -C "D:\dev\my-project"

# 使用终端 TUI 界面
.\ferro.exe --tui

# 纯文本终端交互（适合 SSH / 无图形环境）
.\ferro.exe --no-tui

# 单次只读分析（不修改文件）
.\ferro.exe -C "D:\dev\my-project" --plan -p "梳理项目目录结构与测试入口"

# 指定模型并恢复当前项目最近一次会话
.\ferro.exe -C "D:\dev\my-project" -m "ollama/qwen2.5-coder:7b" --continue

# 在当前目录生成 .ferro/config.toml 模板
.\ferro.exe init --local
```

---

## 从源码构建

### Windows
需要 Git、Rust 工具链（MSVC 需安装 Visual Studio C++ Build Tools）：

```powershell
git clone https://github.com/hanyuana/ferro.git
Set-Location ferro
cargo test --locked
cargo build --release --locked --bin ferro
Copy-Item -Force .\target\release\ferro.exe .\ferro.exe
.\ferro.exe
```

如需同步构建 Windows x64 图形安装程序（将 `ferro.exe` 打包进单个安装器）：

```powershell
cargo build --release --locked --features installer --bin ferro-setup
```

### macOS
需要 Git、Rust 稳定版工具链和 Apple Command Line Tools（`xcode-select --install`）：

```sh
git clone https://github.com/hanyuana/ferro.git
cd ferro
cargo test --locked
cargo build --release --locked --bin ferro
./target/release/ferro -C "$HOME/path/to/project"
```

---

## 常见问题

| 现象 | 排查方式 |
| --- | --- |
| 打开后发消息提示认证失败 | 点击 `设置 → 模型设置` 检查所选模型、接口地址与 API Key；新电脑未配置任何模型密钥时仅展示界面，需配置本机 Ollama 或云端 API 后对话。 |
| 提示上下文已达到模型上限 | 每个会话上下文容量取决于当前所选模型（客户端不会因超限崩溃）；请新建会话或切换到更大上下文窗口的模型继续。 |
| 无法连接本机 Ollama | 确认 Ollama 服务已启动，且在终端运行 `ollama list` 能看到已安装模型。 |
| 只能分析不能修改文件 | 检查左下角权限模式是否处于 `只读规划`，切换为 `帮我批准` 或 `完全访问权限` 即可修改文件。 |
| 自定义中文字体 | 默认自动加载系统字体（Windows `msyh.ttc` / macOS `PingFang.ttc`）；也可通过环境变量 `FERRO_CJK_FONT` 指定自定义 TTF/TTC 字体路径。 |

---

## 平台验证范围

- **Windows x64（已实机验证）**：无边框纯原生 `egui` (`glow`) 窗口、多尺寸图标与独立图形安装器、目录选择器、回车/`Shift+回车` 与中文 IME 防误发、Ollama/云端模型切换、四档权限审批、流式停止、历史检索与回滚，全部 18 项单元测试通过。
- **macOS（代码已适配，待在物理 Mac 实机验收）**：已适配 `native-tls`、`eframe` (`glow`)、`rfd` 原生目录框与 `PingFang.ttc` 字体，尚未在物理 Mac 上执行打包与 GUI 实测。

## 许可证

本项目基于 [MIT License](Cargo.toml) 开源。
