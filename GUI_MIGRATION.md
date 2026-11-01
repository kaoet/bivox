# GUI 迁移计划：从 CLI 工具到纯 GUI 应用

> **状态**：草案 v0.10，**M0 与 M4 已完成**（wgpu 后端已恢复），下一步 M1（或先做 D0 工具链：§11）
> **最后更新**：2026-11-01
> **目标产物**：Windows 单机图形界面应用 `kaols.exe`（便携模式），外加一个独立的百炼音色调试 CLI（仅开发自用）

---

## 0. 这份文档怎么用

- **D（Decision）** = 已确认的设计决定。
- **Q（Question）** = 历史上的开放问题，现已全部确认；结论见第 4 节索引与第 3 节对应 D 项。
- **M（Milestone）** = 里程碑，建议按 M0 → M5 推进。

---

## 1. 目标与非目标

### 1.1 目标（v1 必须达到）

1. 双击 `kaols.exe` 即用：不需要命令行、不需要 `cargo run`、不依赖当前工作目录。
2. **纯便携**：程序目录自包含，配置、缓存、日志都在 exe 旁边；整个文件夹拷走再拷回来即可迁移，不写系统目录、不写注册表。
3. 功能覆盖：实时识别→合成、设备选择、**配置里的自定义音色管理**（编辑器，见 D11）。
4. 无控制台也能排查问题：错误在界面上可见，关键日志落盘。
5. 可维护：UI 代码与业务代码分离，核心逻辑保持纯函数、现有测试继续通过。

### 1.2 非目标（v1 明确不做）

- 跨平台（只针对 Windows 10/11 x64）。
- 安装包/MSI/自动更新/系统级注册（便携模式，zip 分发）。
- 原生控件观感、主题定制、动画。
- 多用户、远程访问、服务化。
- 会话历史持久化、字幕导出、日志导出。

### 1.3 附带开发工具（不属于 GUI 产品的一部分）

- `admin.exe`：**独立的命令行调试工具**，只用于管理百炼账号里已有的自定义音色（列出/删除）。
- **读 `config.toml`**：默认读当前工作目录的 `./config.toml`，`-c/--config` 可覆盖；只取 `[api] key` 与 `[api] workspace_host`（见 D19）。
- **不读写 `voice_cache.json`**、不提供 enroll（复刻/上传）、不做任何 prefix 过滤。
- 不随发布包分发，仅本地 `cargo build --release --bin ...` 自用（已定）。

---

## 2. 事实基线（2026-11-01 核实）

| 项目 | 现状 |
|---|---|
| 代码规模 | 迁移前约 7.4k 行；M0/M4 之后 `src/` 共 7737 行（新增 GUI/admin 约 1.2k，删除 `voice/cli.rs` 931 行） |
| 依赖 | tokio 全异步、**cpal 0.18**（0.15 升上来的：设备名走 `DeviceTrait::description`、`sample_rate()` 返回 u32、`build_*_stream` 收 `StreamConfig` 值）、tokio-tungstenite、reqwest、eframe/egui（默认 wgpu/DX12，可切 glow）、log、chrono；clap/colored 只被调试 CLI 使用 |
| 入口 | `src/main.rs`：eframe GUI 入口；`src/bin/admin.rs`：调试 CLI（M4 已实现） |
| 音色工具结构 | `voice/cli.rs` 已删除；远程列表/删除逻辑在 `voice/admin.rs`（纯逻辑、可用 mock 测试），终端交互与表格在 bin 里 |
| 测试 | 全部模块内 `#[cfg(test)]`；`voice/mock_http.rs` 被 `voice/api.rs`、`voice/cache.rs`、`voice/admin.rs`、`voice/mod.rs` 复用；根目录 `tests/` 为空 |
| 工具链 | `rustc 1.99.1`（2026-10-13，已 `rustup update`）；egui 0.37 的 MSRV 为 1.95，余量充足 |
| 链接器 | `.cargo/config.toml` 固定了 MSVC 链接器路径，不要删除或改写 |
| 工作区 | `master`，干净；无 GUI 代码；`config.toml`、`voice_cache.json` 已被 `.gitignore` 忽略 |
| 文档 | `README.md` 180 行，以 CLI 用法为主，需改写 |

外部依赖版本（docs.rs 核实）：

| 组件 | 版本 | 许可 | 说明 |
|---|---|---|---|
| [eframe/egui](https://docs.rs/eframe/0.37.0/eframe/) | 0.37.0（2026-10-15） | MIT / Apache-2.0 | 纯 Rust 立即模式；`App::ui(&mut self, ui, frame)` 为当前签名 |
| [egui_kittest](https://docs.rs/egui_kittest/0.37.0/egui_kittest/) | 0.37.0 | MIT / Apache-2.0 | 无头 UI 测试 + 快照 + MP4 录制 |
| [egui_mcp](https://github.com/rerun-io/kittest_inspector) | 随 inspection 协议 | — | MCP server，让 AI agent 读写运行中的界面 |

关键能力（官方 CHANGELOG，已在 0.35–0.37 落地）：

- **中文/emoji 字体**：0.37 起原生 eframe 通过 `egui_system_font_provider`（fontique）按需加载系统字体，中文和彩色 emoji 开箱即用。
- **AI 调试**：eframe 的 `inspection` feature 开启后，`EGUI_INSPECTION=1` 启动会在 5719 端口监听，egui_mcp 可读 Accessibility 树、注入输入、截图。

---

## 3. 已确认的设计决定

### D1 ✅ GUI 框架：egui + eframe 0.37.0，版本钉死

```toml
[features]
# 渲染后端二选一：默认 wgpu（DX12），截图核对时用 glow 变体（见 D9）
default = ["renderer-wgpu"]
renderer-wgpu = ["eframe/wgpu"]
renderer-glow = ["eframe/glow"]

[dependencies]
eframe = { version = "=0.37.0", default-features = false, features = [
    "default_fonts", "system_fonts", "accesskit", "clipboard", "links", "inspection",
] }
```

- **两个后端都能用**：默认 `cargo build` 用 wgpu；`cargo build --no-default-features --features renderer-glow` 产出 OpenGL 版（唯一支持 `EFRAME_SCREENSHOT_TO` 截图的组合）。
- **理由**：纯 Rust、单 exe、零运行时依赖；立即模式使 UI 与状态同处一个函数，最适合"控制面板 + 日志 + 字幕"这类工具；inspection + kittest 让 AI 能"看见并操作"界面。
- **不启用 `persistence`**：窗口状态不持久化（D17 决定），避免它默认写系统配置目录、破坏便携性。
- **MSRV**：egui 0.37 要求 Rust ≥ 1.95；本机 1.99.1，满足。
- **注意**：0.37 的 `eframe::App` 方法是 `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame)`，网上教程与模型生成代码多为旧的 `update(&mut self, ctx, frame)`，见 D15。
- **已排除**：iced、slint、Tauri、windows-rs 裸 Win32 / gtk-rs（理由见 v0.1 讨论：架构开销、DSL 双语言调试、多技术栈、冗长/依赖重）。

### D2 ✅ 交付形态：单一二进制、纯便携、无安装器

- 主程序只有 `kaols.exe`（debug 版保留控制台便于看日志，release 隐藏）。
- 移除主程序的全部 CLI 参数、stdin 交互、`-c/--config`。
- 分发：`cargo build --release` 后 zip 打包 **`kaols.exe` + `README.md`**（默认配置模板已编译进 exe，见 D18，不再分发 `config.example.toml`）；**不要**把开发机 `config.toml`（含 API Key）打进包；**不要**打包调试工具。
- 升级方式：覆盖 exe 与文档，旁边的 `config.toml` / `voice_cache.json` / `logs\` 不动。

### D3 ✅ 配置模型与写回范围（已定）

- `config.toml` 仍是唯一事实源；GUI 不引入第二种配置格式。
- **API Key 明文存放**在 `config.toml`（已定），设置页与 README 明确提示"该文件含密钥，勿分享/上传/随包分发"。
- **GUI 可写的字段（仅这些）**：
  1. `[api] key`
  2. `[audio] input_device` / `output_device`
  3. `[tts] voice`（从"内置默认 / 已定义的自定义音色"里选）
  4. `[tts.custom_voices]` 整个结构（增删音色、增删样本、改显示名、改语言、改音频路径、调整样本顺序）
- **其余字段只读**（`[asr]`、`[tts] model`、`[vad]`、`[chunking]`、`[api] workspace_host` 等）：设置页展示当前值 + 「打开配置文件所在目录」+「重新加载」，用户手改 TOML 后点重新加载。
- 写回机制：用 **`toml_edit` 定点修改**原文件文本，保留注释与格式；每次写回前先用 `AppConfig::from_toml_str` 校验新文本，校验失败**不落盘**；写盘前把原文件备份为 `config.toml.bak`。
- 已知限制：整体重写 `[tts.custom_voices]` 时，该表**内部的注释会丢失**（表外注释保留）。v1 接受，README 提示；后续可做逐项编辑。

### D4 ✅ 便携路径：所有文件都在 exe 同目录

```
<exe 所在目录>\
├── kaols.exe
├── config.toml              # 不存在则首启动生成模板（D18）
├── config.toml.bak           # 每次 GUI 写回前的备份
├── voice_cache.json          # 由配置里的相对路径决定，默认同目录
├── logs\kaols-YYYY-MM-DD.log
├── crash.log
└── README.md（随包分发；默认配置模板已内嵌进 exe，不再随包提供 config.example.toml）
```

规则（按优先级写死，不留例外）：

1. **不读 `%APPDATA%`**、不读注册表、不读当前工作目录。
2. **不读任何环境变量**来覆盖配置路径（主程序；调试工具按 D19 读工作目录的 `config.toml`，属独立工具，不受此条约束）。
3. `voice_cache.json`、样例音频等相对路径仍以 **`config.toml` 所在目录**为基准（沿用 `AppConfig::resolve_path`，配置与 exe 同目录，效果一致）。
4. **exe 目录必须可写**：启动做一次写探测（建临时文件再删除）。失败显示整页错误——"程序目录不可写，请把整个文件夹放到可写位置（如 `D:\Apps\kaols`），不要放在 `Program Files`"。
5. **开发期**：`cargo run` 的 exe 在 `target\debug\`，用 `scripts/dev-run.ps1` 把仓库根 `config.toml` 复制到 `target\<profile>\` 后再运行。
6. 便携模式的代价（接受）：备份目录 = 备份 API Key + 音色缓存；README 写清。

### D5 ✅ 线程模型：eframe 占主线程，tokio 跑在后台

- `main()`：先建多线程 tokio runtime 并持有（不要用 `#[tokio::main]`，会占住主线程导致窗口起不来）；`Runtime::handle().clone()` 存入 App 状态，按钮回调里 `handle.spawn(...)`；最后在主线程调用 `eframe::run_native(...)`。
- UI 线程只做绘制与事件泵；**绝不**在 UI 线程 `.await`、阻塞读文件、做网络请求。
- 保留现有 `Arc<AtomicBool>` 停止标志（不引入 `tokio-util`），由 `PipelineSession::stop()` 置位。

### D6 ✅ 事件总线：`PipelineEvent` + 有界通道 + `request_repaint`

- 后台 → 界面统一走 `tokio::sync::mpsc`（与现有代码一致；`crossbeam-channel` 也已依赖，二选一）。
- UI 每帧 `try_recv()` 排空；后台发送后调 `ctx.request_repaint()`（`egui::Context` 为 `Send + Sync` 克隆句柄），避免轮询。
- 契约见第 7 节。

### D7 ✅ 日志体系：`log` 门面 + 内存环形缓冲 + 落盘

- 引入 `log = "0.4"`（egui 已依赖，不增加体积）。
- 自写 `LogSink`（不引 tracing）：同时写 ①界面日志面板环形缓冲（**上限 5000 行**，见 D16）②`<exe 目录>\logs\kaols-YYYY-MM-DD.log`（保留 7 天）。
- `colored` 不再用于 GUI；分级颜色改用 `egui::RichText::color`（ERROR 红、WARN 黄、INFO 默认）。`colored` 保留给调试 CLI 工具（终端输出）。
- 主程序内 `println!` / `eprintln!` 全部改为 `log::*!` 或 `PipelineEvent`。

### D8 ✅ 错误与崩溃必须可见（含 `let _ =` 使用准则）

**原则**：用户可感知的错误不得静默。`let _ =` 只在"忽略是正确的"路径上允许，且必须同时满足：① 失败不改变正确性；② 代码里有 `// best-effort: <原因>` 注释；③ 必要时计数或低频日志。不满足的，一律转 `PipelineEvent::Error` 或 `log::error!`。

**现有代码里明确忽略错误的位置（2026-11-01 排查，非测试代码）**：

| 类别 | 位置 | 为什么允许忽略 | 迁移动作 |
|---|---|---|---|
| 收尾 / 幂等清理 | `client/ws.rs:103`（Ping）、`:273`（读尽）、`:287/:367`（drop）、各 client 收尾时发 finish/close 帧 | 连接/任务已在收尾，失败后没有后续动作可做 | 保留，补 `// best-effort` 注释 |
| 实时音频回调 | `audio/capture.rs:48`（`try_send` 满则丢包）、`audio/playback.rs`（队列空填静音） | 音频回调不能阻塞，丢弃优于卡顿 | 保留；丢弃计数低频上报（`ws.rs:232 drain_and_reset` 已是这个做法） |
| 可选信息降级 | `pipeline/orchestrator.rs:99/102`、`audio/device.rs:18/20/34/36`（设备名读取失败→占位名） | 只影响显示文案 | 保留；UI 上显示"未知设备"而不是空白 |
| 事件通道 best-effort | `client/ws.rs:242`（重置信号 `try_send`） | 通道满/断开时丢失，下一轮事件会覆盖 | 保留 |
| 临时文件清理 | `voice/mod.rs:468`（删除百炼临时上传文件失败） | 不影响音色可用性 | `eprintln!` 改 `log::warn!` |
| 终端输出 | `voice/cli.rs:413`（`stdout().flush()`） | 终端专用 | M4 已随 `voice/cli.rs` 删除（新的调试 bin 自己保留终端交互） |

**必须改的（不属于"可以忽略"）**：

- `pipeline/orchestrator.rs:196/223`：后台任务出错只 `eprintln!` 就结束——GUI 里 stderr 不可见，必须转 `PipelineEvent::Error` 并更新会话状态为 `Failed`。
- `pipeline/orchestrator.rs:255`：`tx_tts.send().is_err()` 之后 `break` 是已处理逻辑（下游关闭→结束调度），事件化时保留这个语义，不要改成忽略。
- 测试代码里的 `.ok()`（大量 `remove_file().ok()`）不受此约束。

**其余要求**：

- `std::panic::set_hook`：写 `<exe 目录>\crash.log`（含 backtrace）+ 置全局"崩溃信息"，下一帧以顶部红色错误条展示。
- 启动阶段致命错误（配置无效、目录不可写、显卡初始化失败）显示整页错误界面，不直接退出。
- 新代码引入 `let _ =` 必须在 code review / AI 约定中被质疑一次（见 D15）。

### D9 ✅ 渲染后端：默认 wgpu（DX12），glow 作为截图/兜底变体

**结论（v0.9 改定）**：升级 `cpal` 0.15 → **0.18** 后，wgpu 后端可以正常编译与运行，恢复为默认。

原因回顾（已解决）：`wgpu 30.0.1` 的 `gpu-allocator 0.28` 声明 `windows = ">=0.53, <=0.62"`，
而旧 `cpal 0.15.3` 固定 `windows 0.54`，Cargo 为去重把 gpu-allocator 的 `windows` 也解析成 0.54，
与 wgpu-hal 的 0.62 不一致 → DX12 后端跨版本类型不匹配、编译失败。
`cpal 0.18.2` 已改用 `windows ^0.62`，依赖图里 0.54 彻底消失，gpu-allocator 与 wgpu-hal 对齐。

- **默认**：`renderer-wgpu`（DX12）。本机实测：启动后 wgpu 枚举到 4 个适配器（Vulkan/DX12 的 RTX 5070 Ti、`Microsoft Basic Render Driver`、GL），进程稳定运行。
- **可选**：`--no-default-features --features renderer-glow` 出 OpenGL 变体。它有两个用途：
  1. **截图核对**：`eframe` 的 `__screenshot` 只实现了 glow 路径（wgpu 侧是 `assert!` 未实现）；
  2. 万一某台机器/远程桌面 DX12 初始化失败，用这个变体兜底。
- **cpal 0.18 带来的代码适配**（已改）：`DeviceTrait::name()` → `description()?.name()`；
  `sample_rate()` 返回 `u32`（不再 `.0`）；`build_input_stream/build_output_stream` 收 `StreamConfig` 值（用 `supported_config.config()`）。
- 影响面：`src/app.rs` 按 feature 选择 `eframe::Renderer`；`scripts/dev-run.ps1 -Glow` 可直接跑 OpenGL 变体。

### D10 ✅ 字体：不打包字体，依赖系统字体按需加载

- egui 0.37 默认 `system_fonts` 会从 Windows 系统字体补中文字形，无需手动加载 `msyh.ttc`。
- M0 验收确认中文标签/日志正常；个别机器字体缺失时退回手动注册 `C:\Windows\Fonts\msyh.ttc`（`FontData::index` 支持 TTC，取 0 号字面）。

### D11 ✅ 音色管理：GUI 只编辑 `config.toml`，完全不碰远程与缓存

- **GUI 的音色页 = 配置编辑器**，数据来源只有 `AppConfig.tts.custom_voices`：
  - 列出 `[tts.custom_voices.<名>]` 及每个样本（语言 + 音频路径）；
  - 新增/删除音色、修改显示名（`name`）、新增/删除样本、选语言（下拉来自 `language::CLONING_LANGUAGES`）、选音频文件（`rfd` 文件选择框）、上移/下移样本（**第一个样本是默认声线**，顺序有语义）；
  - 音频路径存法：选中文件在 exe 目录内 → 存**相对路径**（保证整个目录可搬迁）；在目录外 → 存绝对路径并提示"可能破坏便携性"；
  - 只做本地校验（结构完整、语言合法、音频文件存在性警告），**不发任何网络请求**。
- **远程音色的注册由运行期自动完成**：`voice::resolve_active_voice` 在会话启动时按 (`model`, 音频 SHA256) 查缓存，未命中则上传复刻。所以用户在 GUI 里加完音色，**下次开始会话时自动注册**，无需手动 enroll（原 `voices enroll` 功能因此删除）。
- **`voice_cache.json` 由运行期自行维护**，GUI 不读不写；在 GUI 里删除音色配置后，缓存里的孤儿条目无害（键是音频哈希+模型），v1 不提供清理。
- 删除远端音色不在 GUI 范围内 → 由独立调试工具负责（D19）。
- 当前生效音色 `[tts] voice` 也在这个页面选择：下拉包含"内置默认音色名"与每个自定义音色的 `custom:<键名>`，写回 TOML。

### D12 ✅ 设备枚举与精确匹配：GUI 只存完整设备名

- 现状 `list_devices()` 只打印（device.rs 16–47），`get_input_device(Some(name))` 是不区分大小写的**子串匹配**（device.rs 49–64）。
- 新 API（结构化）：
  - `input_device_names() -> Result<Vec<String>>`
  - `output_device_names() -> Result<Vec<String>>`
  - `default_input_name() / default_output_name() -> Option<String>`
- **匹配规则改为精确相等**：`get_input_device(Some(name))` 只在设备名完全相同时命中；不做模糊匹配、不做兼容回退。
- **GUI 下拉框保存完整设备名字符串**，写回 `[audio] input_device` / `output_device`。
- 下拉第一项固定「系统默认」，选中时配置值为空（`None`）。
- 配置值与当前枚举对不上（换设备/旧配置）时：启动报明确错误"未找到输入设备 `<名字>`"，设置页标红并提示重新选择；**不自动回退默认设备**。
- 热插拔：v1 只提供「刷新设备」按钮，不做自动监听。

### D13 ✅ 优雅退出：先停音频再关窗

1. 拦截关闭（`ViewportCommand::CancelClose`）；
2. 置停止标志、drop `AudioCapture`、停 ASR/TTS 任务、`playback.interrupt()`；
3. 完成后 `ViewportCommand::Close` 真正退出。
- `PipelineSession` 持有 `AudioCapture`（现 `orchestrator.rs` 用 `let _capture` 保活，逻辑搬进 session）。

### D14 ✅ 可测试性：核心逻辑纯函数 + kittest 守门

- `egui_kittest` 作为 dev-dependency（`=0.37.0`），守门用例 3 条：
  1. 配置无效时设置页显示具体错误（Query 到错误文本）；
  2. 点「开始」状态变运行中、点「停止」回到空闲；
  3. 投递 N 条日志事件后日志面板行数增长。
- 纯函数（路径解析、`config_edit` 的 TOML 改写、音色草稿→TOML 映射、事件→UI 状态 reducer）必须带单测。
- 现有 `voice/mock_http.rs` 继续用于调试工具与 `voice/api.rs` 的测试。

### D15 ✅ 版本与 AI 协作约定

- `eframe`、`egui_kittest` 钉 `=0.37.0`；不自动跨小版本。
- `AGENTS.md` 增补：

  ```markdown
  ## GUI 约定
  - eframe/egui 固定 0.37.0，API 以 https://docs.rs/eframe/0.37.0/ 为准。
  - `eframe::App` 的绘制方法是 `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame)`；
    不要写旧版 `fn update(&mut self, ctx: &egui::Context, frame: ...)`。
  - 禁止在 UI 线程 await / 阻塞；后台通信走 events 通道 + `ctx.request_repaint()`。
  - `let _ =` 仅用于 best-effort 收尾 / 实时回调 / 可选信息降级，且必须带 `// best-effort: 原因`；
    用户可感知的错误一律转 `PipelineEvent::Error`（见 D8）。
  - 构建门禁同时覆盖两个 bin：`cargo build --bins`。
  - 每轮改动必须 `cargo fmt --check && cargo build && cargo test` 全绿。
  ```

- 调试：`EGUI_INSPECTION=1` + egui_mcp（`cargo install egui_mcp`；DSH 的具体接法见 §11）。

### D16 ✅ 字幕与日志留存

- 日志面板上限 **5000 行**；字幕上限 **500 句**，均环形裁剪。
- 字幕**只支持单句复制**（每行一个复制按钮），不提供"复制全部"、不导出。
- 不保存历史会话到磁盘。
- 日志面板不提供复制/导出。

### D17 ✅ 启动 / 窗口 / 实例行为

- **启动后不自动监听**：初始状态 `Idle`，用户点「开始」才申请麦克风（已定）。
- **窗口状态不持久化**：每次启动回到默认尺寸 1100×720、居中；用户可自由调整，退出不保存（已定）。
- **单实例**：第二个实例激活已有窗口后退出。用命名 Mutex（`windows-sys::CreateMutexW`），名字由 exe 路径哈希生成——不同目录的两份便携副本可同时运行（已定）。
- **不做托盘图标、不做全局热键**（已定）。
- 布局：最小窗口 900×600；窄窗口时左右分栏自动堆叠。

### D18 ✅ 首次运行

- 启动时若 `<exe 目录>\config.toml` 不存在：写入一份内置默认模板（**`[api] key` 保持注释状态**，使校验明确提示"未配置 API Key"），然后进「设置」页并高亮 Key 输入框。
- 模板来源：仓库根 `config.example.toml` 通过 `include_str!("../config.example.toml")` 在**编译期内嵌**，它是模板的唯一事实源（该文件里 `key` 那一行改成注释）。
- 因为模板已进二进制，**发布 zip 不再包含 `config.example.toml`**（见 D2）；用户首启生成的 `config.toml` 本身就带全部注释字段说明，README 只需列关键字段与示例。
- 模板生成后可被用户手改或 GUI 写回；目录不可写时按 D4-4 显示整页错误。

### D19 ✅ 独立调试 CLI 工具：百炼音色管理

- 新二进制 `admin`（`src/bin/admin.rs`），**只做三件事**：列出百炼账号下的自定义音色、删除指定 voice_id、删除全部音色。
- **读 `config.toml`**：默认读**当前工作目录**的 `./config.toml`，可用 `-c/--config <path>` 指定其它路径；只取 `[api] key` 与 `[api] workspace_host`（复用 `AppConfig::load` + `voice::api_from_config`），其余字段照常校验。
- 因为主程序是便携的，通常应在 **exe 所在目录**运行该工具（这样默认的 `./config.toml` 就是便携目录里的那份）；否则用 `-c` 指向便携目录的配置。配置其它字段无效时，工具会按同一套校验拒绝启动（与 GUI 行为一致）。
- **绝不读写 `voice_cache.json`**；不提供 enroll（复刻/上传）；**不提供任何 prefix 过滤或筛选逻辑**（只按 voice_id 精确操作）。
- 功能边界（已定）：
  - `list`：完整分页校验，输出 voice_id / 前缀 / 语言等远程字段；
  - `delete --voice-id <id>`：可重复指定多个；
  - `delete-all [-y]`：批量删除，默认要求交互确认。
- 逻辑放在 `src/voice/admin.rs`（lib 内，可用 `voice/mock_http.rs` 做单测），bin 只做参数解析与终端输出；保留 `colored` 做终端彩色（调试工具专用）。
- **不随发布包分发**（已定），需要时本地构建。
- 注意（与 GUI 的交互）：如果删掉的 voice_id 仍被 `voice_cache.json` 引用，GUI 下次合成会收到"音色不存在"。v1 的处理是文档说明——删除 `voice_cache.json` 或用 GUI 重新触发复刻即可；不做自动恢复（见 R16）。
- **实现状态（M4 已完成）**：`src/voice/admin.rs` + `src/bin/admin.rs`，命令为
  `admin [-c config.toml] list | delete --voice-id <id> [-y] | delete-all [-y]`；
  已手工验证帮助/配置缺失/非交互确认三条路径，7 个 mock 测试覆盖列表与删除结果分类。

---

## 4. 问题索引（全部已确认，无待定项）

> 没有需要你继续拍板的问题了。下表只为回溯"这条决定是从哪来的"。

| 编号 | 原先的问题 | 结论 | 落点 |
|---|---|---|---|
| 原 Q1 | 设置页写回 `config.toml` 的范围 | C：只写 Key / 设备 / `[tts] voice` / `[tts.custom_voices]`，用 `toml_edit` 保留注释 | D3 |
| 原 Q2 | API Key 存哪 | A：明文放 `config.toml` + 界面与 README 提示 | D3 |
| 原 Q3 | 音色管理形态 | GUI = 纯配置编辑器（不碰远程/缓存）；另建独立调试 CLI 管理百炼音色 | D11、D19 |
| 原 Q4 | 启动是否自动监听 | A：手动点「开始」 | D17 |
| 原 Q5 | 托盘 / 全局热键 | A：v1 不做 | D17 |
| 原 Q6 | 单实例 | A：做，命名 Mutex（按 exe 路径哈希） | D17 |
| 原 Q7 | 字幕/日志留存 | 日志 5000 行、字幕 500 句；仅单句复制；不存历史 | D16 |
| 原 Q8 | 首次运行 | B：自动生成模板 + 引导填 Key | D18 |
| 原 Q9 | 窗口状态 | 不持久化（因此不启用 eframe `persistence`） | D1、D17 |
| 工具 Q1 | 调试工具凭据来源 | 读 `config.toml`（默认工作目录 `./config.toml`，可 `-c` 指定），取 `[api]` | D19 |
| 工具 Q2 | 调试工具功能边界 | `list` / `delete --voice-id` / `delete-all -y`；**无 prefix 逻辑** | D19 |
| 工具 Q3 | 是否随发布包分发 | A：不分发 | D19 |

---

## 5. 删除 / 迁移清单（精确到符号）

| 位置 | 处理 |
|---|---|
| `Cargo.toml` | 删 `clap` 在主程序的使用（依赖保留，仅调试工具用）；`colored` 同理保留给调试工具；加 `eframe`、`log`、`chrono`（已完成），M1/M3 再加 `toml_edit`、`rfd`；**cpal 0.15 → 0.18**（v0.9，见 D9），新增 `renderer-wgpu`/`renderer-glow` feature |
| `src/audio/device.rs`、`capture.rs`、`playback.rs`、`pipeline/orchestrator.rs` | cpal 0.18 适配：`description()?.name()`（新增 `device_name` 辅助）、`sample_rate()` 返回值、`config()` 代替 `&into()`（v0.9 已完成） |
| `src/config.rs:2` | 删 `use clap::{Parser, Subcommand};` |
| `src/config.rs:48-71` | 删 `CliArgs` |
| `src/config.rs:73-87` | 删 `Command` |
| `src/config.rs:89-130` | 删 `VoicesAction` |
| `src/main.rs` | 重写为 GUI 入口（第 6 节） |
| 新增 `src/lib.rs` | 把现有模块改为 `pub mod`，供两个 bin 共用（`voice/api` 等） |
| `src/voice/cli.rs`（931 行） | **M4 已删除**：薄壳、`enroll`、`delete_by_voice_config_with`（配置+缓存耦合）都不要；远程列表/删除逻辑迁到 `voice/admin.rs`，交互与表格在 `src/bin/admin.rs`，测试用 `voice/mock_http.rs` 重写 |
| 新增（M4） | `src/voice/admin.rs`（`list_remote_voices` / `delete_remote_voice` / `delete_all_remote_voices` + 7 个 mock 测试）、`src/bin/admin.rs` |
| `src/audio/device.rs:16-47` | `list_devices()` 打印版删除，改为返回 `Vec<String>`（D12） |
| `src/audio/device.rs:49-81` | 模糊匹配改为精确相等（D12） |
| `src/audio/playback.rs:42-53` | `interrupt()` 从 `#[allow(dead_code)]` 变为正式 API |
| `src/pipeline/orchestrator.rs:104-160` | 启动横幅 `println!` → `PipelineEvent` |
| `src/pipeline/orchestrator.rs:250-253` | chunk `println!` → `PipelineEvent::TtsChunk` |
| `config.example.toml` | 改为**内嵌模板源**：`[api] key` 行改成注释；顶部说明改为"程序首次启动会自动生成 `config.toml`（内容即本文件），本文件仅作模板源，不随包分发"；删除 `--config` 与"当前工作目录"描述 |
| `src/config.rs` 测试段 | 删除 `cli_*` 三个用例；`example_config_is_valid` 改为 `..._template_without_key`（M0 已做） |
| `README.md` | 改写为 GUI + 便携模式说明（配置/缓存/日志/备份/安全提示） |
| `.gitignore`（可选） | 若把 exe 放仓库根，追加 `/logs/`、`/crash.log`、`/config.toml.bak` |
| 新增 | `scripts/dev-run.ps1`、`scripts/package.ps1`、`src/config_edit.rs`、`src/voice/editor.rs`、`src/bin/admin.rs`（M4 已完成） |

> 保留不动：`client/`、`pipeline/chunker.rs`、`language.rs`、`voice/api.rs`、`voice/cache.rs`、`voice/mock_http.rs`、`audio/` 的采集与播放、全部现有测试（除 `voice/cli.rs` 测试需迁移）。
>
> **M0 已完成**：`config.rs` 的 CLI 类型与 `cli_*` 用例、`main.rs` 的 CLI 分发。
> **M4 已完成**：`voice/cli.rs` 整体删除，逻辑迁入 `voice/admin.rs` + 新调试 bin。

---

## 6. 目标代码结构

```
src/
├── lib.rs               # pub mod audio/client/config/events/language/paths/pipeline/voice/...
├── main.rs              # GUI 入口：runtime、panic hook、日志、run_native
├── bin/
│   └── admin.rs             # 调试 CLI（list/delete），参数解析 + 终端输出
├── app.rs               # KaolsApp：状态、事件泵、Tab 分发、退出流程
├── events.rs            # PipelineEvent / SessionState
├── paths.rs             # 便携路径 + 写探测 + 首启模板（D4/D18）
├── log_sink.rs          # log 门面 → 内存环形 + 文件
├── config_edit.rs       # toml_edit 定点写回 + 校验 + .bak（D3）
├── gui/
│   ├── mod.rs
│   ├── toolbar.rs       # 开始/停止、设备下拉、状态灯
│   ├── realtime_tab.rs  # 当前句 + 历史字幕（单句复制）
│   ├── voices_tab.rs    # 音色配置编辑器（D11）
│   ├── settings_tab.rs  # 配置展示/校验/Key 输入/重载
│   ├── log_panel.rs     # 日志面板（虚拟化滚动）
│   └── widgets.rs       # 设备 ComboBox、确认 Modal、错误条
├── config.rs            # 去掉 clap；其余不动
├── audio/               # device.rs：返回列表 + 精确匹配
├── client/              # 不动
├── pipeline/
│   ├── chunker.rs
│   ├── orchestrator.rs
│   └── session.rs       # PipelineSession
├── language.rs          # 不动
└── voice/
    ├── mod.rs, api.rs, cache.rs, mock_http.rs   # 不动
    ├── admin.rs         # 调试工具逻辑（可测，复用 mock_http）
    └── editor.rs        # 音色草稿模型：config ↔ 草稿 ↔ TOML 片段（纯函数 + 测试）

scripts/dev-run.ps1      # 开发期复制 config.toml 到 target\<profile>\ 再运行
scripts/package.ps1      # 打包 zip（不含 config.toml、不含调试工具）
```

---

## 7. 接口契约（草案）

### 7.1 事件

```rust
// src/events.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState { Idle, Starting, Running, Stopping, Failed }

#[derive(Debug, Clone)]
pub enum PipelineEvent {
    StateChanged(SessionState),
    Devices { input: String, output: String },
    VoiceResolved { display: String, languages: Vec<String> },
    /// ASR 当前句（持续修订，is_sentence_end 表示这句结束）
    Asr { text: String, is_sentence_end: bool },
    /// 已送入 TTS 的 chunk（日志/诊断用）
    TtsChunk { language: String, is_sentence_end: bool, text: String },
    /// 本地 VAD 电平（"正在说话"指示；vad.debug 时才有）
    Level { db: f32 },
    Log { level: log::Level, message: String },
    Error { context: String, detail: String },
}
```

### 7.2 会话

```rust
// src/pipeline/session.rs
pub struct PipelineSession {
    cancel: Arc<AtomicBool>,
    capture: AudioCapture,                 // 必须保活，drop 即停止采集
    playback: AudioPlaybackHandle,
}

impl PipelineSession {
    /// 启动：解析音色（可能触发上传复刻）→ 建设备 → 起 ASR/TTS 任务 → 起调度循环
    pub async fn start(config: AppConfig, events: mpsc::Sender<PipelineEvent>) -> Result<Self>;
    /// 「停止播放」（复用 AudioPlaybackHandle::interrupt）
    pub fn interrupt_playback(&self);
    /// 优雅停止（D13）
    pub async fn stop(self) -> Result<()>;
}
```

### 7.3 配置写回（D3）

```rust
// src/config_edit.rs
/// 对原始 TOML 文本做定点修改：只动指定字段，保留其余内容与注释。
pub fn set_api_key(original: &str, key: &str) -> Result<String>;
pub fn set_audio_devices(original: &str, input: Option<&str>, output: Option<&str>) -> Result<String>;
pub fn set_active_voice(original: &str, voice: &str) -> Result<String>;
/// 整体替换 [tts.custom_voices]（该表内部注释会丢失）
pub fn set_custom_voices(
    original: &str,
    voices: &BTreeMap<String, CustomVoiceConfig>,
) -> Result<String>;

/// 统一入口：改写 → 用 AppConfig::from_toml_str 校验 → 备份 .bak → 写盘
pub fn save_config(path: &Path, original: &str, next: &str) -> Result<()>;
```

### 7.4 音色草稿（D11）

```rust
// src/voice/editor.rs
/// GUI 编辑态：从配置读出的可编辑结构（与 TOML 解耦，便于单测）
pub struct VoiceDraft {
    pub name: String,                 // 配置键名
    pub display_name: String,         // [..].name
    pub samples: Vec<SampleDraft>,    // 有序；第 0 个是默认声线
    pub is_active: bool,              // 是否被 [tts] voice 引用
}
pub struct SampleDraft { pub language: String, pub audio: PathBuf }

pub fn drafts_from_config(cfg: &AppConfig) -> Vec<VoiceDraft>;
pub fn drafts_to_config(original: &str, drafts: &[VoiceDraft], active: Option<&str>) -> Result<String>;
```

### 7.5 设备枚举（D12）

```rust
impl AudioDeviceManager {
    pub fn input_device_names(&self) -> Result<Vec<String>>;
    pub fn output_device_names(&self) -> Result<Vec<String>>;
    pub fn default_input_name(&self) -> Option<String>;
    pub fn default_output_name(&self) -> Option<String>;
    /// 精确匹配完整设备名；None = 系统默认
    pub fn get_input_device(&self, exact_name: Option<&str>) -> Result<Device>;
    pub fn get_output_device(&self, exact_name: Option<&str>) -> Result<Device>;
}
```

### 7.6 路径（D4/D18）

```rust
pub struct AppPaths {
    pub exe_dir: PathBuf,       // std::env::current_exe()?.parent()
    pub config_file: PathBuf,   // <exe_dir>/config.toml
    pub log_dir: PathBuf,       // <exe_dir>/logs
    pub crash_log: PathBuf,     // <exe_dir>/crash.log
}
impl AppPaths {
    pub fn resolve() -> std::io::Result<Self>;
    pub fn ensure_writable(&self) -> std::io::Result<()>;
    /// 配置不存在时写内置模板（key 保持注释），返回是否新建
    pub fn ensure_default_config(&self) -> std::io::Result<bool>;
}
```

---

## 8. 任务分解

> 每阶段结束跑：`cargo fmt` → `cargo fmt --check` → `cargo build --bins` → `cargo test`（AGENTS.md 硬性要求）。
> 预估按"熟悉代码 + AI 辅助"，乐观/保守两列。

### M0 拆分 lib + 空壳可跑通（已完成 ✅）

- [x] `src/lib.rs` 拆出（现有模块 `pub mod`），`main.rs` 改为 GUI 入口
- [x] `Cargo.toml`：加 `eframe = "=0.37.0"`（glow + inspection）、`log`、`chrono`；主程序不再用 `clap`/`colored`
- [x] `main.rs`：`windows_subsystem`、tokio runtime、panic hook、日志初始化、`run_native`
- [x] `app.rs`：`impl eframe::App`，工具栏 + 3 个 Tab 空壳（实时/音色/设置）
- [x] `paths.rs`：便携路径 + 写探测 + 首启模板（D18）
- [x] `log_sink.rs`：内存环形缓冲 + 按天文件日志 + 日志面板
- [x] `scripts/dev-run.ps1`
- [x] 中文渲染验证（`EFRAME_SCREENSHOT_TO` 截图确认）；渲染后端定为 glow（D9）
- [x] 关闭按钮走 D13 流程（暂无音频，逻辑先搭好）
- [x] 确认没有任何系统目录写入（只有 `<exe>\config.toml`、`<exe>\logs\`）

**实施结果**：
- 构建/测试：`cargo fmt --check` 通过、`cargo build --bins` 通过、`cargo test` 113 个用例全绿。
- 截图核对：`target/shot-m0-first-run.png`（首启 → 设置页 + 配置无效错误条）、`target/shot-m0-loaded.png`（配置有效 → 实时页）。当时用 glow 构建截图（wgpu 默认是 v0.9 才恢复的，截图命令见 §10）。
- 遇到的坑：① `wgpu` 依赖冲突（见 D9）；② `egui::Grid` 触发 multi-pass，eframe 的 `__screenshot`
  在 multi-pass 下会抓到空帧 —— 已把设置页改成普通横排行，并把这条写进第 10 节。
**预估**：乐观 0.5 天 / 保守 1 天（实际约 2 小时，其中大半花在 wgpu 冲突排查上）

### M1 配置读取、校验与写回

- [ ] `config_edit.rs`：`set_api_key` / `set_audio_devices` / `set_active_voice` + 校验 + `.bak`（带单测：改写后注释仍在、其它字段不变）
- [ ] 设置页：字段展示 + 校验错误列表 + 「打开配置文件所在目录」+「重新加载」
- [ ] API Key 输入与保存（D3）、安全提示文案
- [ ] 设备下拉（D12 新 API）+「刷新设备」+「系统默认」+ 不一致时红字
- [ ] 配置无效 / 目录不可写时整页错误

**验收**：删配置后启动自动生成模板并提示填 Key；改坏 key 后界面报错；保存 Key 后 `config.toml` 注释未丢；设备名写成不存在值时明确报错且不回退
**预估**：乐观 1 天 / 保守 1.5 天

### M2 实时管线事件化（改动最大）

- [ ] `events.rs` 契约落地
- [ ] `orchestrator.rs` 拆分 → `session.rs::PipelineSession::start/stop`
- [ ] 全部 `println!`（横幅、chunk、错误）→ 事件/日志
- [ ] `app.rs` 事件泵：字幕、状态灯、日志
- [ ] 工具栏开始/停止（三态）+ 「停止播放」接 `interrupt()`
- [ ] 字幕：当前句 + 历史句（上限 500、单句复制）
- [ ] 停止后设备释放（连续 3 次开始/停止）
- [ ] 失败路径：无 Key、设备名不匹配、设备被占用、网络失败

**验收**：说话 → 字幕刷新、chunk 入日志、听到合成音；停止→开始可恢复，重复 3 次不掉设备
**预估**：乐观 1.5 天 / 保守 2.5 天

### M3 音色配置编辑器（D11）

- [ ] `voice/editor.rs`：`drafts_from_config` / `drafts_to_config`（纯函数 + 单测）
- [ ] `config_edit.rs::set_custom_voices`（整体替换该表 + 校验 + `.bak`）
- [ ] 音色页：列表、新增/删除音色、改显示名、样本增删改、上移/下移（首样本=默认声线标注）
- [ ] 语言下拉（`language::CLONING_LANGUAGES`）+ 音频文件选择（`rfd`）+ 文件存在性警告
- [ ] 音频路径相对/绝对策略（exe 目录内→相对路径）
- [ ] 生效音色 `[tts] voice` 下拉（内置/自定义）
- [ ] 保存后：提示"下次开始会话时生效"（会话运行中时），未运行时可自动重载
- [ ] kittest 增加"音色页存在并可添加样本行"用例

**验收**：新增一个自定义音色并保存 → TOML 结构正确、注释保留（表外）；删除音色后配置无残留；非法语言/缺文件有提示且不允许保存
**预估**：乐观 1.5 天 / 保守 2 天

### M4 调试 CLI 工具（D19）（已完成 ✅）

- [x] `src/bin/admin.rs`：`-c/--config`（默认 `./config.toml`）、`list`、`delete --voice-id`（可重复、`-y` 跳过确认）、`delete-all -y`、终端表格与确认
- [x] `AppConfig::load` + `voice::api_from_config` 取 `[api]` 凭据（不碰 cache、不 enroll）
- [x] `voice/admin.rs`：`list_remote_voices`（强制完整分页，否则报错）/ `delete_remote_voice`（区分 Deleted 与 AlreadyMissing）/ `delete_all_remote_voices`（返回成功/已不存在/失败三分类报告）；**不做任何 prefix 过滤**
- [x] 新增 7 个 mock HTTP 测试（列表完整/不完整、删除三种结果、空 voice_id 不发请求、批量结果分类）
- [x] **不做**：enroll、读写 `voice_cache.json`、prefix 筛选
- [x] `voice/cli.rs`（931 行）整体删除；`Cargo.toml` 加 `default-run = "kaols"`，`cargo run` 仍默认进 GUI

**实施结果**：
- `cargo run --bin admin -- --help / list -c missing.toml / delete --voice-id x`（非交互终端）三条路径已手工验证：
  帮助正常、配置缺失给出可读错误、无 `-y` 时拒绝并提示（不会误发删除请求）。
- `cargo build --bins` 通过；`cargo test` **110 个用例全绿**（删掉旧 cli 的 9 个用例、新增 admin 7 个）。
- 用法：在便携目录内运行（默认读 `./config.toml`），或 `-c <path>` 指定。
**预估**：乐观 0.5 天 / 保守 1 天（可与 M2/M3 穿插）

### M5 打磨与发布

- [ ] 应用图标（`ViewportBuilder::with_icon` + exe 资源，可选 `winres`）
- [ ] 单实例（D17，命名 Mutex 按 exe 路径哈希）
- [ ] 日志轮转与清理（保留 7 天）
- [ ] `[profile.release]`：`lto = "thin"`、`strip = true`、`codegen-units = 1`；记录产物大小
- [ ] `README.md` 改写（GUI 使用、便携模式、配置字段、安全提示、调试工具单独说明）
- [ ] `AGENTS.md` 增补 GUI 约定（D15）
- [ ] `scripts/package.ps1` → `dist/kaols-<ver>-win64.zip`（只含 `kaols.exe` + `README.md`；不含 `config.toml`、`config.example.toml`、调试工具）

**验收**：干净机器解压到可写目录即用；整个目录拷到另一台机器后配置与缓存仍生效；zip 内无密钥
**预估**：乐观 0.75 天 / 保守 1 天

### 总览

| 阶段 | 内容 | 乐观 | 保守 | 依赖 |
|---|---|---|---|---|
| M0 | 拆 lib + 空壳 + 日志 + 便携路径 + 崩溃 | 0.5 | 1.0 | — |
| M1 | 配置读取/校验/写回 | 1.0 | 1.5 | M0、D12 |
| M2 | 实时管线事件化 | 1.5 | 2.5 | M0 |
| M3 | 音色配置编辑器 | 1.5 | 2.0 | M1（复用 config_edit） |
| M4 | 调试 CLI 工具 | 0.5 | 1.0 | M0（可穿插） |
| M5 | 打磨与发布 | 0.75 | 1.0 | 全部 |
| **合计** | | **5.75 天** | **9.0 天** | |

新增代码量预估：1.6k–2.2k 行；删除/迁移约 400–500 行（`voice/cli.rs` 的配置/缓存逻辑与薄壳）。

---

## 9. 风险登记

| # | 风险 | 概率 | 影响 | 应对 |
|---|---|---|---|---|
| R1 | egui 0.37 预热不足：AI 生成旧 API 代码 | 高 | 中 | 钉版本 + AGENTS.md + 报错贴 `docs.rs/0.37.0` |
| R2 | 部分机器/远程桌面下 GPU 初始化失败 | 低 | 中 | 默认 wgpu（DX12，M0 已在本机验证到适配器）；失败时用 glow 变体构建兜底，错误写入日志/crash.log |
| R3 | 去掉控制台后错误静默 | 中 | 高 | panic hook + 文件日志 + 界面错误条（D8）；禁止 `let _ =` |
| R4 | 便携目录不可写（Program Files、只读盘） | 中 | 高 | 写探测 + 整页提示（D4-4） |
| R5 | `toml_edit` 改写把配置写坏 / 丢表外注释 | 中 | 高 | 写前 `from_toml_str` 校验、写前 `.bak`、单测覆盖"其它字段不变"、失败不落盘 |
| R6 | `[tts.custom_voices]` 整体替换丢失表内注释 | 高 | 低 | 文档与 README 说明；后续可做逐项编辑 |
| R7 | 多 bin 共用 lib，改动影响另一侧 | 中 | 中 | 门禁用 `cargo build --bins`；共享模块（`voice/api.rs`）改动必须两边都测 |
| R8 | 配置写回后与运行中会话不一致 | 中 | 中 | 会话运行中保存时提示"下次开始生效"，不做热重载 |
| R9 | 麦克风被占用 / 设备热插拔 | 中 | 中 | 明确报错 + 刷新按钮；精确匹配失败时报出配置值（D12） |
| R10 | 长跑内存增长 | 中 | 中 | 日志 5000 行、字幕 500 句环形裁剪（D16） |
| R11 | 关窗时后台线程未收尾 | 中 | 中 | D13 + M2 "连续 3 次开始/停止"验收 |
| R12 | 自定义音色启动上传慢/失败 | 中 | 中 | 后台任务 + Starting 状态 + 超时提示 |
| R13 | 双开抢麦克风、并发写缓存 | 低 | 中 | 单实例（D17） |
| R14 | 立即模式每帧重算导致高 CPU | 低 | 低 | 仅事件到达时 `request_repaint`；`ScrollArea::show_rows` 虚拟化 |
| R15 | 便携目录被备份/分享，泄露 Key 与音色缓存 | 中 | 中 | README + 设置页提示；zip 不含 config.toml |
| R16 | 调试工具删掉仍被 `voice_cache.json` 引用的远端音色 → GUI 合成报"音色不存在" | 中 | 中 | 文档说明：删除 `voice_cache.json` 或改配置触发重新复刻；v2 可做"TTS 报 not-found 时失效缓存并重试一次" |
| R17 | cpal 0.15→0.18 升级后只验证了设备枚举（能列出输入/输出设备），**录音/播放链路未做真机回归** | 中 | 高 | M2 首次接管线时必须实测：麦克风采集、扬声器播放、断连重连；失败优先看采样格式/重采样分支 |

---

## 10. AI 协作与质量门禁

1. **版本钉死**：`eframe = "=0.37.0"`、`egui_kittest = "=0.37.0"`（dev）。
2. **每轮门禁**：`cargo fmt --check && cargo build --bins && cargo test`，失败把完整错误贴回模型；不允许 `--no-verify`。
3. **文件规模**：`gui/` 下每个文件 ≤ 300 行，便于 AI 精确修改与 review。
4. **运行期观测**：`EGUI_INSPECTION=1` + egui_mcp 让 agent 自己点一遍（DSH 接法见 §11）；kittest 做回归。
5. **截图核对**：wgpu 后端不支持 `EFRAME_SCREENSHOT_TO`（上游未实现），需要临时用 glow 变体构建：
   `$env:EFRAME_SCREENSHOT_TO="xxx.png"; cargo run --no-default-features --features "renderer-glow,eframe/__screenshot"`
   —— 无交互截屏后自动退出。⚠️ 但 `egui::Grid` 之类会触发 multi-pass（`request_discard`）的组件
   会让它抓到**空帧**（M0 实测）；需要截图核对的页面避免用 Grid，或用 kittest 语义断言替代。
6. **提交粒度**：一个里程碑一组 commit；提交前按 AGENTS.md 跑全套检查。
7. **回归测试**：M2 起补 kittest 三条守门用例（见 D14）。

---

## 11. 开发工具链：egui_mcp（D0，非产品里程碑）

**时机**：建议现在就做（M1 之前）。它不改变产品代码，只影响开发期自测方式；M1/M3 的 UI 验证最需要它。最晚在 M2 接实时管线前完成即可。

### 11.1 应用侧（已就绪，无需改代码）

- `eframe` 的 `inspection` feature 已在 D1 的依赖里启用（运行期默认关闭，只认环境变量）。
- 启动方式：`$env:EGUI_INSPECTION=1; .\scripts\dev-run.ps1` → 应用在 `127.0.0.1:5719` 开 inspection 端口；不设置该变量则完全关闭。
- 说明：这是**开发期调试开关**，不属于 D4「不读环境变量」的配置路径规则（不改变配置/路径语义，release 也只在显式设置时才开端口）。
- 截图限制：inspection 的 screenshot 需要窗口可见（最小化/完全遮挡时拿不到帧）；另外 eframe 的 `EFRAME_SCREENSHOT_TO` 在 wgpu 下不可用，静态截图仍走 glow 变体（§10）。

### 11.2 DSH（DeepSeek Harness）侧：一次性改动

DSH 自带 MCP 客户端插件 `@deepseek-ai/dsh-mcp-client`（bundle 内置，v0.2.1-rc.5），不用装插件，只需要：

1. **安装 MCP server**（crates.io 已有 0.3.0；依赖 `egui ^0.37` / `egui_inspection ^0.37`，与本项目 0.37 匹配）：
   ```powershell
   cargo install egui_mcp
   ```
   产物在 `%USERPROFILE%\.cargo\bin\egui-mcp.exe`（该目录已在 PATH）。

2. **加一条 profile patch**，追加到 `%USERPROFILE%\.dsh\profiles\desktop\cordis.patch.yml` 末尾：
   ```yaml
   - id: mcp-egui
     name: '@deepseek-ai/dsh-mcp-client'
     config:
       serverName: egui
       transport: stdio
       command: 'C:\Users\kaoet\.cargo\bin\egui-mcp.exe'
       args: []
   ```
   （`serverName` 会变成工具名前缀；可用 `env` 覆盖环境变量，见该插件 README。）

3. **重启 DSH**（Cordis profile 在启动时加载）。重启后**新会话**的工具列表里会出现 `mcp__egui__*`；当前会话不会热加载。

4. egui-mcp 是 stdio server，不需要端口/防火墙；它主动去连本机 5719。

### 11.3 使用流程

1. 终端：`$env:EGUI_INSPECTION=1` 后启动 GUI（保持窗口可见）。
2. 新会话里让 agent：`attach` → 读 `widget_tree` / `get_widget` → `click` / `type` / `press keys` → `screenshot` 核对。
3. 工具名形如 `mcp__egui__attach`、`mcp__egui__click`、`mcp__egui__screenshot`（以实际工具列表为准）。

### 11.4 与其它验证手段的分工

| 需求 | 手段 |
|---|---|
| CI/离线的语义回归（按钮状态、错误文案） | `egui_kittest`（D14） |
| agent 操作真实窗口、复现交互 bug、截图核对 | egui_mcp + inspection（本节） |
| 静态截图产物（可保存/对比） | glow 变体 + `EFRAME_SCREENSHOT_TO`（§10） |

### 11.5 注意

- inspection 只监听 `127.0.0.1`（eframe 默认）；不要把端口暴露到外网。
- `attach` 失败时按顺序排查：GUI 是否带 `EGUI_INSPECTION=1` 启动 → 5719 是否被占用 → egui_mcp 版本是否仍匹配 egui 0.37。
- 如果不想动 DSH 配置：可以退化为在仓库里写一个直连 inspection 协议的小 devtool（shell 调用），成本更高，不推荐。
- 重启 DSH 会中断当前会话；建议安排在不影响手头工作的时候做。

---

## 12. 变更记录

| 版本 | 日期 | 说明 |
|---|---|---|
| v0.1 | 2026-11-01 | 初稿：确定 egui 方案、纯 GUI 形态、事件化架构、M0–M5、Q1–Q10 |
| v0.2 | 2026-11-01 | 删除"打字→TTS"相关全部内容；工具链更新为 1.99.1；路径改为纯便携（exe 同目录，不读 `%APPDATA%`/CWD/环境变量）；设备改为完整名精确匹配；编号重排 |
| v0.3 | 2026-11-01 | 九项决策落定（原 Q1–Q9，现已并入 D 项）：原 Q1-C（`toml_edit` 写回，范围扩到 Key/设备/生效音色/自定义音色）、原 Q2-A（Key 明文）、原 Q3（GUI 音色页=配置编辑器，不碰远程与缓存；新增独立调试 CLI 只做百炼列出/删除、无 enroll）、原 Q4-A（手动开始）、原 Q5-A（无托盘/热键）、原 Q6-A（单实例）、原 Q7（日志 5000 行/字幕 500 句、仅单句复制、不存历史）、原 Q8-B（首启生成模板）、原 Q9（窗口状态不持久化，故移除 `persistence`）；新增 D16–D19、`src/lib.rs` 拆分与 `voice/admin.rs` 迁移表；里程碑增至 M0–M5，工时 5.75/9.0 天 |
| v0.4 | 2026-11-01 | 调试工具的三项定案：① 凭据改为**读 `config.toml`**（默认工作目录 `./config.toml`，`-c/--config` 可覆盖，只取 `[api]`；撤销 v0.3 的"不读配置"）；② 功能固定为 `list` / `delete --voice-id` / `delete-all -y`，**明确不做任何 prefix 过滤**；③ 不随发布包分发。§4 由"待拍板问题"改为"问题索引"，全部 12 项决策可回溯；D19 重写，新增 R16（远端删除导致缓存失效）；确认无待定问题，可开工 M0 |
| v0.5 | 2026-11-01 | ① §1.3 修正：调试工具**读 `config.toml`**（默认工作目录，`-c` 可覆盖），删掉"不读配置"的表述；② D8 由绝对禁令改为分级准则，列出当前 6 类明确忽略错误的位置（Ping/收尾帧、实时音频 `try_send`、设备名降级、重置信号、临时文件清理、终端 flush）及各自迁移动作，必须改的只剩 `orchestrator.rs:196/223` 的 `eprintln!`；新增 `// best-effort:` 注释约定并写入 D15；③ D18 明确以 `config.example.toml` 为 `include_str!` 模板源，因此发布 zip **不含** `config.example.toml`（同步修正 D2、§5 迁移表、M5 打包清单） |
| v0.6 | 2026-11-01 | **M0 实施完成**：新增 `src/lib.rs` / `main.rs`（GUI 入口）/ `app.rs` / `paths.rs` / `log_sink.rs` / `gui/*` / `scripts/dev-run.ps1`；删除 `config.rs` 的 CLI 类型与用例；`config.example.toml` 改为内嵌模板（key 注释）。**渲染后端由 wgpu 改为 glow**（D9：wgpu 30.0.1 + gpu-allocator 0.28 被 cpal 的 windows 0.54 拖成版本冲突，wgpu-hal DX12 编译失败）；设置页去掉 `egui::Grid`（multi-pass 会让 eframe `__screenshot` 抓到空帧，已写入 §10）。验证：fmt/build/test（113 用例）全绿，两张截图确认中文与布局正常 |
| v0.7 | 2026-11-01 | **M4 实施完成**：新增 `voice/admin.rs`（`list_remote_voices` 强制完整分页、`delete_remote_voice` 区分已删除/本来不存在、`delete_all_remote_voices` 三分类报告，7 个 mock 测试）与调试 bin（`-c/--config`、`list`、`delete --voice-id`、`delete-all -y`，交互确认需输入 YES）；**删除 `voice/cli.rs` 931 行**（enroll 与配置/缓存耦合逻辑一并移除）；`Cargo.toml` 加 `default-run = "kaols"`；顺手把 `AppConfig::load` 的错误提示从"复制 config.example.toml"改成"程序会在 exe 同目录自动生成"。验证：`cargo build --bins` + `cargo test`（110 用例）全绿，CLI 帮助/配置缺失/非交互确认三条路径手工验证 |
| v0.8 | 2026-11-01 | 按你的要求把调试 CLI 改名为 **`admin`**：`src/bin/kaols-voice-admin.rs` → `src/bin/admin.rs`，clap `name = "admin"`，文档与命令示例同步（`cargo run --bin admin -- list`）；功能与参数不变 |
| v0.9 | 2026-11-01 | **cpal 0.15 → 0.18 升级，wgpu 后端恢复为默认**（D9 重写）：cpal 0.18 改用 `windows ^0.62`，依赖图里 0.54 消失，`gpu-allocator` 与 `wgpu-hal` 对齐，wgpu 编译/运行通过（本机枚举到 4 个适配器，DX12 RTX 5070 Ti 可用）。代码适配：`DeviceTrait::name()`→`description()?.name()`（新增 `audio::device::device_name`）、`sample_rate()` 返回 u32、`build_*_stream` 收 `StreamConfig` 值（`supported_config.config()`）。Cargo 增加 `renderer-wgpu`（默认）/`renderer-glow` 两个 feature，`app.rs` 按 feature 选渲染器；glow 变体保留用于截图核对（wgpu 不支持 `EFRAME_SCREENSHOT_TO`）与 OpenGL 兜底；`dev-run.ps1 -Glow`。验证：两种变体均构建通过，GUI 以 wgpu 启动 6s 稳定运行，cpal 0.18 设备枚举正常（中英文设备名正确），`cargo test` 全绿。新增 R17（音频链路未做真机回归） |
| v0.10 | 2026-11-01 | 新增 **§11 开发工具链：egui_mcp（D0）**：明确时机为 M1 之前（不影响产品代码）；应用侧已就绪（`inspection` feature + `EGUI_INSPECTION=1` 开 5719，截图需窗口可见）；DSH 侧只需三步——`cargo install egui_mcp`（crates.io 0.3.0，依赖 egui/egui_inspection ^0.37，与本项目匹配）、在 `~/.dsh/profiles/desktop/cordis.patch.yml` 追加 `@deepseek-ai/dsh-mcp-client` 插件条目（DSH 内置该插件，无需另装）、重启 DSH 后新会话生效；给出与 kittest/静态截图的分工与排障清单 |
