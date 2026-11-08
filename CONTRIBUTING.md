# 贡献指南（CONTRIBUTING）

本文件面向**开发与贡献者**（以及需要维护本仓库的 AI 编码代理），是项目开发流程、编码规范与发布流程的**唯一权威来源**。

- 为什么要这样设计：[`docs/decisions.md`](docs/decisions.md)（`D1`–`D32`）
- 模块、数据流与算法：[`docs/architecture.md`](docs/architecture.md)
- 用户使用说明：[`README.md`](README.md)
- AI 代理入口：[`AGENTS.md`](AGENTS.md)（只做导航，不重复本文件规则）

项目当前尚未正式发布（`Cargo.toml` 版本 0.1.0），不需要兼容旧配置/旧缓存格式。

---

## 1. 环境要求

- Windows 10/11 x64；
- Rust（rustup 安装），MSRV **1.95**（egui 0.37 要求），当前开发工具链为 **rustc 1.99.1**；
- Visual Studio Build Tools（含 MSVC 链接器与 Windows SDK；安装时勾选“使用 C++ 的桌面开发”/C++ 生成工具）；
- PowerShell（`scripts/` 下是 `.ps1` 脚本）；
- 运行实时功能需要百炼 API Key。

### 关于 `.cargo/config.toml`

仓库提交了 `.cargo/config.toml`，其中固定了当前开发机的 MSVC `link.exe` 与 Windows SDK 库路径。换机器构建时：

- 如果报错找不到 `link.exe` / 找不到 `kernel32.lib` 等，按本机 Visual Studio 与 Windows SDK 的实际路径修改该文件；
- 这是本机环境适配，**提交前用 `git diff` 确认没有把与自己环境相关的路径改动混进业务提交**；
- 不要在没有确认的情况下删除它；如果只是临时让 cargo 自动发现 MSVC，可先移出工作区，构建完恢复。

---

## 2. 构建与运行

```bash
cargo build --bins        # 构建两个 bin：kaols（GUI）与 admin（调试 CLI）
cargo run                 # 启动 GUI（默认 wgpu 后端，debug 版保留控制台）
cargo run --bin admin -- list   # 音色与热词列表调试 CLI，见第 9 节
cargo test                # 全部单元测试
```

开发期启动脚本：

```powershell
.\scripts\dev-run.ps1         # 构建并启动 debug GUI
.\scripts\dev-run.ps1 -Release
.\scripts\dev-run.ps1 -Glow   # glow/OpenGL 后端（截图核对时用）
.\scripts\dev-run.ps1 -NoRun  # 只准备 target 目录，不启动 GUI（脚本自检用）
```

`dev-run.ps1` 会做两件事：把仓库根 `config.toml` 复制到 `target\<profile>\`（因为程序只读 exe 同目录的配置），并把副本里的 `[cache] file` 指回仓库根 `cache.json`，让 debug/release 共用同一份缓存（音色复刻与 ASR 预编译热词），避免反复上传复刻、重建热词列表。

注意：

- `cargo run` 不带 `dev-run.ps1` 时，`target\debug\` 下需要有可用的 `config.toml`；
- 本机的 `config.toml`、`cache.json`、`logs/`、`crash.log`、`dist/`、`target/` 都不提交（`.gitignore` 已覆盖）；
- release 构建配置在 `Cargo.toml`（`lto = "thin"`、`strip = true`、`codegen-units = 1`），exe 图标由 `build.rs` 写入。

---

## 3. 测试

- 测试全部是模块内 `#[cfg(test)]` / `#[tokio::test]`，不放根目录 `tests/`。
- 测试使用本地 mock，**不访问真实百炼网络**：
  - WebSocket 协议：测试代码内起本地 `TcpListener`，手写握手/事件；
  - HTTP（音色接口）：`src/voice/mock_http.rs` 的手写 mock 服务。
- 不为“旧配置/旧缓存不再兼容”写测试；文件格式变化时让旧文件解析失败、备份并重建即可。

### 新增测试的落点

| 逻辑类型 | 测试方式 |
|---|---|
| 纯函数（配置校验、TOML 写回、音色草稿、字幕 reducer、语言判定、分块） | 同模块 `#[cfg(test)]` 单测 |
| WebSocket 协议/重连 | 测试内本地 `TcpListener` mock |
| 音色 HTTP 接口 | `voice/mock_http.rs` |
| UI 交互 / 视觉 | 不写自动化 UI 测试（不使用 `egui_kittest`，D14）；用 egui_mcp 运行期观测 + 截图 |
| 发布前整体行为 | 人工冒烟（见第 8 节） |

带过滤运行：

```bash
cargo test language      # 只跑名字匹配的测试
cargo test -- --nocapture
```

---

## 4. 调试

### 日志与崩溃

- GUI 底部日志面板：内存保留最近 5000 行，面板渲染最近 500 行；
- `logs\kaols-YYYY-MM-DD.log`：按天文件，保留 7 天；
- `crash.log`：panic 钩子写入（含 backtrace），界面也会显示崩溃横幅；
- `[vad] debug = true`：每个音频块打印 VAD 电平、噪声底、阈值与发送决策，用于标定阈值。

### egui_mcp：让 agent/开发者在真实窗口中操作与截图

前提：eframe 的 `inspection` feature 已启用；运行期只有设置 `EGUI_INSPECTION=1` 才开启 inspection 端口（默认不监听）。

1. 安装 MCP server（crates.io，依赖 `egui ^0.37`，版本需与本项目匹配）：

   ```powershell
   cargo install egui_mcp
   ```

   产物通常在 `%USERPROFILE%\.cargo\bin\egui-mcp.exe`。

2. 在 MCP 客户端里注册 `egui-mcp`（stdio）。DSH 用户可在 `%USERPROFILE%\.dsh\profiles\desktop\cordis.patch.yml` 末尾追加：

   ```yaml
   - insert:
       - id: mcp-egui
         name: '@deepseek-ai/dsh-mcp-client'
         config:
           serverName: egui
           transport: stdio
           command: 'C:\Users\<你>\.cargo\bin\egui-mcp.exe'
           args: []
   ```

   ⚠️ 必须用 `- insert:` 包装。只写 `- id: ...` 会被 Loader 当作“覆盖已有行”；目标行不存在时只打一条 stderr 警告并跳过，表现为“完全没生效、也看不到报错”。

3. 重启 DSH（插件配置在启动时加载），新会话工具列表里出现 `mcp__egui__*`。

4. 启动 GUI 并保持窗口可见：

   ```powershell
   $env:EGUI_INSPECTION=1
   .\scripts\dev-run.ps1
   ```

   然后 `attach` → 读控件树 → 点击/输入 → 截图。默认端口 `127.0.0.1:5719`，只监听本机。

限制与注意：

- 窗口不可见（最小化/完全遮挡）时拿不到帧；
- 0.37 下通过 inspection 注入点击 ComboBox 弹层里的选项可能不生效（真实鼠标不受影响）；
- inspection 只是开发期调试开关，不改变配置/路径语义，也不违反便携模式的“不读环境变量”规则（它只开端口）。

任意支持 stdio MCP 的客户端都可接入，不限于 DSH。

### 静态截图

wgpu 后端不支持 `EFRAME_SCREENSHOT_TO`，需要临时构建 glow 变体：

```powershell
$env:EFRAME_SCREENSHOT_TO="shot.png"
cargo run --no-default-features --features "renderer-glow,eframe/__screenshot"
```

注意：`egui::Grid` 等会触发 multi-pass（`request_discard`）的组件可能让截图抓到空帧；需要截图核对的页面避免使用这类布局。

---

## 5. 提交前检查（强制）

**任何 `git commit` 之前都必须执行以下流程，没有例外：**

```bash
cargo fmt              # 1. 格式化整个项目（不是只格式化本次改动的文件）
cargo fmt --check      # 2. 退出码必须为 0
cargo build --bins     # 3. 两个 bin 都要编译通过
cargo test             # 4. 全部测试通过
git diff --check       # 5. 无空白符错误
git add -A
git commit -m "..."    # 6. 以上全部通过后才允许提交
```

约束：

- `cargo fmt --check` 失败时**不允许**提交；
- 不允许使用 `--no-verify` 或任何方式绕过检查；
- 如果 `cargo fmt` 顺带改到与本次任务无关的文件，必须把这些纯格式改动一并提交或单独提交，不能为了让 diff 好看而撤销它们（否则全项目 `cargo fmt --check` 会失败）；
- 一个提交只做一件逻辑上的事；提交信息简洁说明“做了什么、为什么”；
- 文档改动也应跑完上述门禁（至少 `cargo fmt --check`、`cargo build --bins`、`cargo test`），保持仓库始终可构建。

---

## 6. 代码与注释规范

### 通用

- 注释、终端提示语、日志文案用**中文**；标识符用英文；提交信息简洁描述“做了什么、为什么”。
- 每个模块顶部写 `//!` 模块文档，说明职责与关键约束；改动行为时同步更新。
- 涉及设计约束的注释使用 `D<n>` 编号引用 [`docs/decisions.md`](docs/decisions.md)，不复制整段理由。
- 注释与代码保持一致：已删除的按钮/API 不要留在注释里。

### GUI 与线程

- `eframe` 固定 `=0.37.0`（egui 系列同版本），API 以 <https://docs.rs/eframe/0.37.0/> 为准；
- 绘制方法是 `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame)`，不要写旧版 `update(ctx, frame)`；
- UI 线程只做绘制和状态更新：禁止 `await`、阻塞 IO、网络请求；后台任务通过 `PipelineEvent` 回传并 `ctx.request_repaint()`；
- `let _ =` 只用于“忽略失败是正确的”路径，且必须带原因注释（如 `// best-effort: ...`）；用户可感知的错误必须转成 `PipelineEvent::Error` / 错误横幅 / `log::error!`；
- `gui/` 下单文件目标 ≤300 行，新增 GUI 逻辑应拆到独立文件。

### 配置与便携模式

- `config.toml` 是唯一配置事实源；GUI 只能写：`[api] key`、`[audio]` 设备、`[asr] model` / `[asr] languages` / `[asr] hotwords`（「语音识别」页）、`[tts] voice` / `[tts] dialect` / `[tts.custom_voices]`、`[translate] enabled` / `target` / `granularity` / `show_source`（「翻译」页）；`[vad]` / `[chunking]` 等仍只读；
- 写回使用 `toml_edit` 定点修改：写前用 `AppConfig::from_toml_str` 校验、写前备份 `config.toml.bak`、无改动不写盘；
- 主程序不读 `%APPDATA%`、注册表、当前工作目录，也不用环境变量改配置路径；
- 同一便携目录只允许一个实例；日志保留 7 天；发布 zip 只含 `kaols.exe` + `README.md`；
- 不要提交 `config.toml`、`config.toml.bak`、`cache.json`、`logs/`、`crash.log`。

### 音色、缓存与热词

- GUI 只编辑配置里的音色与 ASR 热词结构，**不访问百炼接口、不读写 `cache.json`**；远程音色与预编译热词列表的列出/删除只走 `admin` 调试 CLI；
- `[cache] file` 默认 `cache.json`（相对配置文件目录），同时保存音色复刻记录与 ASR 预编译热词 `vocabulary_id`（含热词快照）；删除它会导致下次重新复刻音色、重建热词列表；
- 自定义音色样本是**有序数组**，`samples[0]` 是默认声线；每个样本按（TTS 模型, 音频内容 SHA256）单独复刻/缓存；合成时按本句语言选样本，缺语言回退第一个；
- 不要在每次合成时重新上传/复刻；同一音频在不同模型下分别缓存，切换模型不得覆盖其它模型记录。

### 语言策略

- `[asr] languages` 有序（首个默认）；整份列表原样传给 ASR API 的 `language_hints`（数量上限由服务端处理）；
- 本地语言判定始终使用完整列表；规则见 `src/language.rs`（假名/谚文强证据、纯汉字 ≥6 切中文、拉丁需主导、短文本粘性）；
- 判定在每句首个 TTS chunk 前完成并冻结；不要改成“等整句再判”（会破坏低延迟）；
- 合成始终传目标语言 `language_hints`；非中英语种附带内置 `instruction`；这两者与复刻样本语种无关。
- `[asr] hotwords` 在会话启动时自动同步为百炼预编译热词列表（D31）：首次创建并缓存 `vocabulary_id`，热词变化时更新；更新过频或失败时本次降级并提示，不阻断会话。

### 版本与兼容

- 项目尚未发布：配置/缓存文件里不要引入版本号，不要写旧格式兼容代码；文件格式变化时让旧文件解析失败并自动备份重建即可；
- 不要为“旧配置/旧数据不再兼容”写测试，也不要在文档中描述迁移/兼容说明；
- 依赖版本遵循 [`docs/decisions.md`](docs/decisions.md) D1/D15（eframe/egui 钉 `=0.37.0`，cpal 0.18），不要随意升级 GUI/音频依赖。

---

## 7. 文档维护

### 文档地图与唯一事实源

| 内容 | 唯一来源 | 说明 |
|---|---|---|
| 用户使用、配置摘要、故障排查 | `README.md` | 会随发布 zip 分发，必须自包含 |
| 开发流程、规范、测试、发布、调试 | 本文件 | 其它文档只引用，不复制 |
| AI 代理入口 | `AGENTS.md` | 只做导航与阅读顺序 |
| 模块/数据流/算法 | `docs/architecture.md` | 当前实现的“怎么做” |
| 设计决定与理由 | `docs/decisions.md` | `D1`–`D32`，编号稳定 |
| 配置字段与默认值 | `config.example.toml` | 编译期内嵌为首次启动模板 |
| 语言代码集合 | `src/language.rs` | README 的用户表格据此维护 |
| 模块职责概览 | `docs/architecture.md` | 本文件不重复维护目录树 |
| 人类任务/想法记录 | `TODO.md` | **仅人类使用；AI 编码代理不得读取或修改** |

### 更新规则

- 用户可见行为变化 → 更新 `README.md`；必要时同步 `docs/architecture.md`；
- 模块边界、数据流、协议、算法变化 → 更新 `docs/architecture.md`；
- 新的长期设计取舍 → 在 `docs/decisions.md` 追加 `D<n>`（不重排旧编号），并更新相关源码注释中的 `D` 引用；
- 命令、门禁、代码规范、发布流程变化 → 更新本文件；`AGENTS.md` 保持只引用；
- 配置字段变化 → 更新 `config.example.toml`，README 只保留常用字段摘要；
- 语言列表变化 → 更新 `src/language.rs` 与 README 的对应表格。

### 写作约定

- 文档用中文；“为什么”写在 decisions，“怎么做”写在 architecture，“规范/流程”写在本文件；
- `TODO.md` 是人类维护的任务/想法记录，**AI 编码代理不得读取或修改**；人类可以在其中记录未定稿的计划，但它不是项目规范或事实来源；
- 项目未发布：不要写迁移叙事、历史版本变更记录或旧格式兼容说明；这些信息交给 git 历史；
- 不要在多份文档重复维护同一事实；能引用就引用；
- 修改 `README.md` 时注意它会被打进发布包：仓库内的 `CONTRIBUTING.md`、`docs/`、`AGENTS.md` 不会随包分发，相关说明要明确这一点；
- 新增或移动文档后，检查所有相对链接（GitHub 渲染与发布包场景不同）。

---

## 8. 发布流程

1. 更新 `Cargo.toml` 的 `package.version`；
2. 按第 5 节跑完整门禁；
3. 打包：

   ```powershell
   .\scripts\package.ps1            # 构建 release 并打包
   .\scripts\package.ps1 -NoBuild   # 用现有 target\release\kaols.exe 打包
   ```

4. 检查产物 `dist\kaols-<version>-win64.zip`：
   - 只包含 `kaols.exe` + `README.md`；
   - 不含 `config.toml`、`config.example.toml`、`cache.json`、日志、`admin` 调试工具；
5. 冒烟检查（人工）：
   - 干净目录解压，首次启动生成配置并落在设置页；
   - 填 Key 后实时会话能开始/停止，设备正确打开与释放；
   - 配置保存生效，运行中保存能热重启管线；
   - 自定义音色能注册/复用，缓存跨启动命中；
   - 双开触发单实例并激活已有窗口；
   - 日志、崩溃文件落在 exe 目录；
6. 跨机便携验收：把整个目录复制到另一台机器后配置/缓存仍可用；目前没有自动化测试，需要人工冒烟；
7. `dist/` 是打包产物，已被 `.gitignore` 忽略，不提交。

---

## 9. 调试 CLI（`admin`）

`admin` 只用于本地管理百炼账号下**已有的**自定义音色与预编译热词列表，不随发布包分发。

```bash
cargo run --bin admin -- list
cargo run --bin admin -- delete --voice-id <id> [-y]
cargo run --bin admin -- delete-all [-y]
cargo run --bin admin -- vocab list
cargo run --bin admin -- vocab delete --vocabulary-id <id> [--vocabulary-id <id2>] [-y]
cargo run --bin admin -- -c <path-to-config.toml> list
```

- 默认读**当前工作目录**的 `./config.toml`，`-c/--config` 可指定其它路径；沿用同一套配置校验，只实际使用 `[api] key` 与 `[api] workspace_host`；
- 不读写 `cache.json`、不提供 enroll（上传/复刻）、不做任何 prefix 过滤，只按 voice_id / vocabulary_id 精确操作；
- `list` 与 `vocab list` 要求完整枚举所有页，否则报错（避免半截列表被误认为全部）；
- `delete` 区分“已删除”与“本来就不存在”；`delete-all` 汇总成功/已不存在/失败；`vocab delete` 同样区分“已删除”与“本来就不存在”；
- `vocab delete` 只按 `--vocabulary-id` 删除、可重复传入，**没有 delete-all**（避免误删账号下其它应用创建的列表）；热词列表的创建与更新由主程序在会话启动时自动完成；
- 逻辑在 `src/voice/admin.rs` 与 `src/asr/admin.rs`（可用 mock HTTP 测试），`src/bin/admin.rs` 只做参数解析与终端输出。

如果删除的远程音色仍被 `cache.json` 引用：下次会话启动解析自定义音色时会通过 `query_voice` 校验并自动重新复刻；网络异常时会先沿用缓存（标记为未校验）。
