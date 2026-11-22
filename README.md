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

准备 Git 和 Rust 工具链。使用 Rust 的 Windows MSVC 工具链时，还需要 Visual Studio C++ Build Tools。Ferro 默认借助本机的 Edge 或 Chrome 打开单独的桌面窗口。

```powershell
git clone https://github.com/hanyuana/ferro.git
Set-Location ferro
cargo build --release --locked
cargo test --locked
.\target\release\ferro.exe
```

编译产物位于 `target\release\ferro.exe`。首次打开后选择要工作的目录。也可以直接指定目录：

```powershell
.\target\release\ferro.exe -C 'D:\dev\my-project'
```

当前验证以 Windows 为主。其他平台的桌面入口尚未完成验收，可先从终端模式试用。

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
| 窗口打开了，但发不出消息 | 检查所选模型、API Key 和服务地址。只有客户端时可以打开界面，生成回答仍需要模型。 |
| Ollama 连接被拒绝 | 确认 Ollama 在运行，并检查本机端口和实际模型名。 |
| API 返回 401、404 或模型不存在 | 分别检查密钥、Base URL、模型 ID 和服务账号权限。 |
| 能聊天，但不会修改文件 | 检查模型是否支持工具调用，以及 Ferro 的模式和工具权限。 |
| Windows 下双击未打开桌面 | 检查本机浏览器；可先用 `--tui` 验证核心功能。 |

## 当前验证范围

Windows 上已验证通过本机 Magpie 进行流式对话、保存和重新打开会话，也用本地模拟服务验证了不经过 Magpie 的 OpenAI 兼容流式连接。真实 Ollama、新 Windows 用户首次配置和非 Windows 桌面体验仍待完整验收。

开发验证可以从 `cargo test --locked` 开始。Rust 的包元数据声明了 MIT 许可证。
