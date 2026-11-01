# AGENTS.md

本文件面向在本仓库中工作的 AI 编码代理（Claude Code / Codex / Cursor 等）。人类贡献者也建议遵守同样的约定。

## 强制规则：每次 `git commit` 前必须 `cargo fmt`

**在创建任何 git 提交之前，必须先运行 `cargo fmt`。** 这是硬性要求，没有例外：

```bash
cargo fmt              # 1. 格式化整个项目（不是只格式化本次改动的文件）
cargo fmt --check      # 2. 验证全项目无格式差异，退出码必须为 0
cargo build --bins     # 3. 确认两个 bin（kaols / admin）都编译通过
cargo test             # 4. 确认测试通过
git add -A
git commit -m "..."    # 5. 以上全部通过后才允许提交
```

约束：

- 不允许在 `cargo fmt --check` 失败（退出码非 0）时提交。
- 不允许用 `git commit --no-verify` 之类的参数绕过检查。
- 如果格式化顺带改到了与本次任务无关的文件，必须把这些纯格式改动一并提交或单独提交，不能为了让 diff 好看而撤销它们从而导致全项目 `cargo fmt --check` 失败。
- 提交前用 `git diff --check` 再确认没有空白符错误。

## 常用命令

```bash
cargo build --bins        # 构建 GUI 主程序 + 调试 CLI
cargo run                 # 启动 GUI（默认 wgpu；首次启动在 exe 同目录生成 config.toml）
cargo test                # 全部单元测试（含本地 mock WebSocket / HTTP 服务）

.\scripts\dev-run.ps1         # 开发期启动：把根目录 config.toml 复制到 target\debug 再运行
.\scripts\dev-run.ps1 -Glow   # 同上，但用 glow/OpenGL 后端（截图核对时用）

# 百炼音色调试 CLI（默认读当前目录 config.toml，可用 -c 指定）
cargo run --bin admin -- list
cargo run --bin admin -- delete --voice-id <id> [-y]
cargo run --bin admin -- delete-all [-y]

# 截图核对：wgpu 后端不支持 EFRAME_SCREENSHOT_TO，必须用 glow 变体
$env:EFRAME_SCREENSHOT_TO="shot.png"
cargo run --no-default-features --features "renderer-glow,eframe/__screenshot"
```

运行需要 `config.toml` 中的 `[api] key`。程序只读 **exe 所在目录**的 `config.toml`，
不读当前工作目录 / `%APPDATA%` / 环境变量；首次启动会自动生成带注释的默认模板。

## 项目结构速览

- `src/main.rs`：GUI 入口（eframe），只负责定位便携目录并调用 `app::run`；release 不弹控制台。
- `src/lib.rs`：库入口，暴露全部模块（两个 bin 共用）。
- `src/app.rs`：应用状态、面板布局、事件泵、关闭流程、启动准备（日志/崩溃/默认配置）。
- `src/paths.rs`：便携路径解析 + 内置默认配置模板。
- `src/config_edit.rs`：用 `toml_edit` 定点写回配置（只写 Key / 设备等指定字段），写前校验 + `.bak` 备份。
- `src/log_sink.rs`：`log` 门面 → 内存环形缓冲（界面日志面板）+ 按天文件日志 + panic 钩子。
- `src/gui/`：工具栏、实时/音频/语音合成/设置页签、日志面板、通用部件（每个文件 ≤ 300 行）。
- `src/config.rs`：TOML 配置加载与校验（API Key、业务空间域名、设备、ASR/TTS 模型、
  `[asr] languages`、多语言自定义音色样本、VAD 参数）。
- `src/language.rs`：语言代码集合、合成 `instruction` 映射、ASR 文本脚本统计与
  `LanguageTracker`（强证据 + 粘性 + `han >= 6` 切中文等规则）。
- `src/audio/`：`cpal 0.18` 采集/播放、设备枚举、重采样、本地能量 VAD 门控。
- `src/client/`：
  - `qwen_asr.rs`：百炼 Qwen Audio 实时双向流式识别。
  - `qwen_tts.rs`：Qwen-Audio-TTS 流式语音合成。
  - `ws.rs`：鉴权建连、握手、Ping 保活、断线指数退避重连的公共骨架。
- `src/pipeline/`：`chunker.rs` 流式文本分块，`orchestrator.rs` 编排 ASR -> 语言判定 -> 分块 -> TTS。
- `src/voice/`：自定义（复刻）音色。
  - `api.rs`：调百炼 HTTP 接口（文件上传、音色复刻/查询/列表/删除），不依赖 SDK。
  - `cache.rs`：`voice_cache.json`（模型 -> 音频 SHA256 -> voice_id）读写。
  - `admin.rs`：调试 CLI 的纯逻辑（远程音色列出/删除，可用 mock HTTP 测试）。
  - `mod.rs`：启动时解析 `[tts] voice`，并行注册全部语言样本，返回「语言 -> voice_id」映射。
- `src/bin/admin.rs`：`admin` 调试 CLI——只做百炼音色的列出/删除；不读缓存、不 enroll、无 prefix 过滤。

## GUI 约定

- **版本钉死**：`eframe` / `egui_kittest` 固定 0.37.x，API 以 https://docs.rs/eframe/0.37.0/ 为准。
- `eframe::App` 的绘制方法是 `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame)`；
  **不要**写旧版 `fn update(&mut self, ctx: &egui::Context, ...)`（模型训练数据里几乎全是旧签名）。
- **UI 线程只做绘制与状态更新**：禁止在 UI 线程 `await` / 阻塞 / 做网络请求；
  后台任务通过 channel 回传事件，并调用 `ctx.request_repaint()`。
- **`let _ =` 只用于 best-effort 路径**（收尾、实时音频回调、可选信息降级），且必须带原因注释；
  用户可感知的错误一律转成事件/错误条（见 GUI_MIGRATION.md D8）。
- **配置写回**：只有 API Key、输入/输出设备、`[tts] voice`、`[tts.custom_voices]` 可由 GUI 修改；
  用 `toml_edit` 定点修改、写前校验、写前备份 `.bak`（D3）。
- **音色边界**：GUI 只编辑 `config.toml` 里的音色结构，不访问百炼接口、不读写 `voice_cache.json`；
  远程音色的列出/删除只走 `admin` 调试 CLI（D11/D19）。
- **便携模式**：配置/缓存/日志/崩溃文件都在 exe 同目录，不写系统目录、不读环境变量改配置路径
  （唯一例外是调试用的 `EGUI_INSPECTION`，它只开 inspection 端口）。
- **构建门禁**同时覆盖两个 bin：`cargo fmt --check && cargo build --bins && cargo test`。
- **调试**：`EGUI_INSPECTION=1` 启动 GUI，可接 egui_mcp 让 agent 读控件树/点击/截图（GUI_MIGRATION.md §11）。

## 代码约定

- 注释与终端提示语用中文，标识符用英文；提交信息简洁描述“做了什么、为什么”。
- 涉及 WebSocket 协议、重连、VAD、音色 HTTP 接口等逻辑时，测试使用本地 `tokio` mock 服务（`src/voice/mock_http.rs` 是纯手写的 HTTP mock），不依赖真实百炼网络。
- 自定义音色结构：`[[tts.custom_voices.<名>.samples]]` 是**有序数组**，`samples[0]` 是默认声线；
  每个样本按（模型, 音频内容哈希）单独复刻/缓存；合成时按判定语言选样本，缺语言回退 `samples[0]`。
- 语言策略：`[asr] languages` 有序（首个默认），整份列表原样传给 ASR API 的 `language_hints`
  （数量上限由服务端处理），**本地判定始终使用完整列表**；判定规则见 `language.rs`（假名/谚文强证据、纯汉字 ≥6 切中文、
  拉丁需主导、短文本粘性），不要改成"等整句再判"（会破坏低延迟）。
- 合成时始终传 `language_hints`（目标语言），非中英语种附带内置 `instruction`；这两者与复刻样本语种无关。
- 自定义音色缓存是「模型 -> 样例音频内容 SHA256 -> voice_id」两层结构，**不要让每次合成都重新上传/复刻**；同一音频在不同模型下分别缓存，切换模型不应覆盖其它模型的记录。
- 项目尚未发布：配置/缓存文件里不要引入版本号，也不要写旧格式兼容代码；文件格式变化时让旧文件解析失败并自动备份重建即可。不要为“旧配置/旧数据不再兼容”专门写测试，也不要在文档中描述这类不兼容/迁移说明。
- 不要提交 `config.toml`（已被 `.gitignore` 忽略）；密钥只放在 `[api] key` 中，不再读取环境变量。
- `voice_cache.json` 是本机状态（也被 `.gitignore` 忽略），不要提交。
- `target/` 下的 `logs/`、`crash.log` 是便携模式运行产物，不要提交（已在 `.gitignore`）。
