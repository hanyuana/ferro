# Ferro

Ferro 是用 Rust 编写的轻量 AI 编码助手。它提供桌面对话窗口、终端界面和单次命令行模式，可以围绕一个本地项目读取代码、修改文件、运行测试和查看改动。

Ferro 本身无需注册或登录。生成回答需要一个可用的模型，可以连接本机 Ollama、自己的模型 API，或者使用已配置的 Magpie 网关。

## 功能

- **围绕项目对话**：选择本地工作目录，打开历史会话，用 `@文件` 将工作区文件加入上下文。
- **读写与验证**：搜索代码、修改文件、运行项目命令，查看工具步骤及修改预览。
- **计划与执行**：用只读规划模式先梳理方案，再切换到执行模式修改代码。
- **桌面交互**：可收起侧栏、独立滚动的历史列表、底部输入框、深浅色主题、流式 Markdown 和代码块复制。
- **审阅与回滚**：在改动卡片中检查文件差异。撤销依赖当前运行中的文件快照，重要改动请同时用 Git 审阅和管理。
- **本地记录**：保存对话历史，支持项目记忆和技能说明。使用远程模型时，模型所需的提示词和代码上下文会发往配置的服务端。

## 从源码开始（Windows）

准备 Git 和 Rust 工具链。使用 Rust 的 Windows MSVC 工具链时，还需要 Visual Studio C++ Build Tools（GNU 工具链可直接纯 Rust 编译）。Ferro 默认启动基于 `egui` (`eframe` / `glow`) 的纯原生独立桌面窗口，不启动任何外部浏览器（Edge/Chrome）子进程，也不监听本地网页端口。

```powershell
git clone https://github.com/hanyuana/ferro.git
Set-Location ferro
cargo build --release --locked
cargo test --locked
.\target\release\ferro.exe
```

编译产物位于 `target\release\ferro.exe`。首次打开后可通过左侧栏 `Open project…` 选择要工作的目录，也可以在启动时直接指定目录：

```powershell
.\target\release\ferro.exe -C 'D:\dev\my-project'
```

## 从源码开始（macOS）

需要 Git、Rust 稳定版工具链和 Apple Command Line Tools（未安装时运行 `xcode-select --install`）。在要运行 Ferro 的 Mac 上原生编译：Apple Silicon 使用 `aarch64-apple-darwin` 工具链，Intel 使用 `x86_64-apple-darwin`。

```sh
git clone https://github.com/hanyuana/ferro.git
cd ferro
cargo build --release --locked
cargo test --locked
```

编译产物是 `target/release/ferro`。下面的首次对话路径无需 Ferro 登录或 Magpie，也不会下载模型。

### 使用已有 Ollama 模型开始对话

确认本机 Ollama 服务已经运行；用 `ollama list` 找到**已安装**模型的完整名称（包括标签），替换下面的 `YOUR_INSTALLED_MODEL`。如果列表为空，需自行准备模型或使用下一节已有 API。

```sh
ollama list
./target/release/ferro -C "$HOME/path/to/project" --model 'ollama/YOUR_INSTALLED_MODEL'
```

`-C` 指向已存在的项目目录。默认连接 `http://localhost:11434/v1`，无需 Key。Ferro 直接打开独立原生 `egui` 桌面窗口（无本地 HTTP/SSE 端口，也不调用浏览器）。

点击左侧栏的 `Open project…` 按钮，会通过 `rfd` 打开系统原生文件夹选择对话框；选择目录后将在后台 worker 中切换工作区并重新加载该目录的配置与会话历史；取消则保留当前目录。也可点击顶部 `Model setup` 在窗口内直接调整 `provider/model`、Base URL 与 API Key。

在底部输入框发送“只回复 hello，不调用工具”（点击 `Send` 或按 `Ctrl/Cmd+Enter`）以确认首次对话连通。模型支持工具调用时，再试只读项目分析。也可在终端直接检查同一个模型：

```sh
./target/release/ferro -C "$HOME/path/to/project" --model 'ollama/YOUR_INSTALLED_MODEL' --plan -p 'Reply hello without using tools.'
```

### 使用已有兼容 API 开始对话

在目标项目的 `.ferro/config.toml` 中使用下方“方案 B”的配置，把 Base URL 和模型 ID 替换为服务商实际值。然后在同一终端设置 Key 并启动（`myproxy/my-coding-model` 对应该配置）：

```sh
export FERRO_PROVIDER_KEY='YOUR_EXISTING_API_KEY'
./target/release/ferro -C "$HOME/path/to/project" --model 'myproxy/my-coding-model'
```

发送同样的 hello 消息。Ferro 不需要账号；API 服务商可能需要自己的账号和有效密钥。macOS 全局配置路径为 `~/Library/Application Support/ferro/config.toml`，项目配置为 `<project>/.ferro/config.toml`，后者覆盖前者；`--model` 优先于配置。`--tui` 和 `--no-tui` 仍可用于终端会话。

## 连接模型

### 方案 A：用 Ollama 在本机运行模型

1. 安装并启动 Ollama。运行 `ollama list`，确认本机已经有可用模型。如果列表为空，先用 `ollama pull <模型名>` 拉取一个。
2. 在 Ferro 中打开模型设置，将模型填为 `ollama/<实际模型名>`。也可以用命令行显式指定。模型名应与 `ollama list` 的结果一致。
3. 本机默认服务地址是 `http://localhost:11434/v1`。默认本地连接无需 API Key。

```powershell
ollama list
.\target\release\ferro.exe --model 'ollama/你的模型名'
```

能生成文本只说明对话连通。要让 Ferro 自动读写文件和跑测试，还需选支持工具调用的模型。

### 方案 B：使用兼容的模型 API

使用你持有的 API Key、服务商提供的 Base URL 和实际模型 ID。下面是项目级配置示例。先在**要使用的项目根目录**创建 `.ferro/config.toml`，再启动 Ferro。示例地址和模型名需要替换。

```toml
model = "myproxy/my-coding-model"

[provider.myproxy]
type = "openai"
base_url = "https://example.com/v1"
api_key = "env:FERRO_PROVIDER_KEY"
```

在启动 Ferro 的终端中设置 `FERRO_PROVIDER_KEY`。建议通过环境变量注入密钥，避免把明文 Key 写入仓库。OpenAI 兼容地址通常填到 `/v1`，Ferro 会追加 `/chat/completions`。

内置服务商包括 OpenAI、Anthropic、DeepSeek、Qwen、Moonshot、智谱、OpenRouter 和 Ollama。服务端模型名称和账号权限以你实际可用的配置为准。

### 方案 C：沿用现有 Magpie

本机已通过 OpenCode 配置了 Magpie 时，Ferro 可尝试读取已有配置并导入模型。在模型列表中选择能正常使用的 `magpie/…` 模型即可。没有 Magpie 的机器可以直接使用上面的 Ollama 或 API 方案。

## 使用方式

```powershell
# 打开终端界面
.\target\release\ferro.exe --tui

# 纯文本终端会话
.\target\release\ferro.exe --no-tui

# 在指定项目执行一次只读分析
.\target\release\ferro.exe -C 'D:\dev\my-project' --plan -p '梳理项目结构和测试入口'

# 恢复当前工作目录最近的会话
.\target\release\ferro.exe -C 'D:\dev\my-project' --continue

# 在当前项目生成一份配置示例
.\target\release\ferro.exe init --local
```

建议首次尝试先让 Ferro 解释项目结构。确认工作目录和权限后，再给一个有明确预期的小任务，例如：

> 检查标签解析函数。去掉空标签，按 Unicode 大小写折叠去重，同时保留第一次出现的拼写和顺序。只修改对应源码，运行相关测试并列出结果。

Ferro 会先读全局配置，再应用项目的 `.ferro/config.toml`。命令行的 `--model` 可覆盖配置中的模型。生成的配置示例允许文件编辑和多数命令。想逐项确认时，可在项目配置里设置：

```toml
[permission]
edit = "ask"

[permission.bash]
"*" = "ask"
```

权限规则用于决定允许、询问或拒绝工具调用，不能代替操作系统沙箱。单次命令行模式遇到需要确认的工具默认会拒绝，`--yes` 会统一批准这些请求，仅在可信的测试目录使用。

## 常见问题

| 现象 | 检查方式 |
| --- | --- |
| 窗口打开了，但发不出消息 | 点击顶部 `Model setup` 检查所选模型、API Key 和服务地址。只有客户端时可以打开界面，生成回答仍需要模型。 |
| Ollama 连接被拒绝 | 确认 Ollama 在运行，并检查本机端口和实际模型名。 |
| API 返回 401、404 或模型不存在 | 分别检查密钥、Base URL、模型 ID 和服务账号权限。 |
| 能聊天，但不会修改文件 | 检查模型是否支持工具调用、底部 `PLAN (read only)` 是否被勾选，以及工具权限配置。 |
| 桌面窗口中文显示异常 | Ferro 默认自动加载系统字体（Windows `msyh.ttc`/`simhei.ttf`，macOS `PingFang.ttc`）；也可通过环境变量 `FERRO_CJK_FONT` 指定自定义 TTF/TTC 字体路径。 |
| 无图形环境或远程 SSH 会话 | 原生桌面窗口需要本地图形桌面会话与 OpenGL (`glow`) 支持；在无头或 SSH 环境下请使用 `--tui`、`--no-tui` 或 `-p` 命令行模式。 |

## 当前验证范围

- **Windows（已在实机验证）**：
  - 纯原生 `egui` (`eframe` / `glow`) 独立桌面窗口启动、后台独立 Tokio worker 线程通信、无任何外部浏览器（Edge/Chrome）子进程、无任何本地 UI 端口监听。
  - 首轮流式对话、工具调用与内联权限审批弹窗（`Tool approval`：Allow once / Allow this turn / Reject & stop）、流式生成与工具执行中途点击 `Stop` 取消中断、历史会话全文搜索与重新打开、单轮文件修改快照回滚（`Undo last turn edits`）。
  - 保留 `--tui` 终端界面、`--no-tui` 纯文本 REPL、`-p` 单次非交互执行及可选的本地 Magpie 自动发现。
- **macOS（代码已适配，待在 Mac 实机验收）**：
  - 已配置 macOS 原生 `reqwest` (`native-tls` / `Security.framework`)、`eframe` (`glow`) 原生窗口、`rfd` 原生文件夹选择对话框、`/System/Library/Fonts/PingFang.ttc` 中文字体加载与 macOS 系统目录过滤；
  - 因当前开发与测试环境为 Windows，macOS 原生编译（`aarch64-apple-darwin` / `x86_64-apple-darwin`）、Cocoa/OpenGL 窗口渲染、`rfd` 目录选择对话框以及 Mac 实机上的首次模型对话尚未在物理 Mac 上实测。

开发验证可以从 `cargo test --locked` 开始。Rust 的包元数据声明了 MIT 许可证。
