# 架构与模块设计

面向项目开发与贡献者，描述 kaols 当前实现的结构、数据流、关键算法与边界。

- “为什么这样设计”见 [decisions.md](./decisions.md)；
- 用户可见行为以 `README.md` 为准；
- 本文只描述当前代码，不记录历史版本或迁移过程。改动模块边界、数据流或算法时，请同步更新本文；新的长期设计取舍写到 [decisions.md](./decisions.md)。

---

## 1. 系统概览

kaols 是一个 Windows 10/11 x64 桌面应用：采集麦克风音频，实时送云端 ASR，按标点/语义把识别文本切成小段，再流式送 TTS 合成并播放，做到“边说边出字幕、边合成”。

- 纯 GUI（egui/eframe），便携模式：配置、缓存、日志都在 exe 同目录（D4）；
- 内部全异步：tokio runtime 跑后台任务，eframe 占主线程（D5）；
- 两个二进制：`kaols.exe`（GUI 主程序）与 `admin.exe`（百炼音色调试 CLI，不随发布包分发，D19）；
- 外部依赖：阿里云百炼 DashScope 的 Qwen-Audio 实时 ASR/TTS WebSocket 接口，以及声音复刻 HTTP 接口；
- 翻译模式（`[translate] enabled`，D25）：ASR/打字文本先经 `qwen-mt-flash` 翻译成目标语言，再用目标语言音色合成，字幕以「译文 + 原文」双语呈现（M2，D26；独立 GUI 翻译页是 M3）；
- 没有本地语音模型，运行必须联网并配置 API Key。

### 数据流

```text
麦克风
  └─ cpal 采集（设备原生采样率/声道）
       └─ 单声道 + 重采样到 16kHz PCM16，按 ~100ms 切块
            └─ 本地 VAD 发送门控（可选，默认开）
                 └─ ASR：WebSocket 双向流式 run-task
                      └─ 识别句子事件（text / is_sentence_end / sentence_id）
                           ├─ 语言判定（本地，逐 TTS task）
                           ├─ 流式文本分块（标点/词数阈值）
                           │    └─ TTS：每句一个 run-task，continue-task 流式送文本
                           │         └─ 返回 PCM 22050Hz binary 帧
                           │              └─ 播放队列 + 重采样到设备采样率 → 扬声器
                           └─ 事件泵：字幕 / 状态 / VAD / 错误 → egui 界面
```

实时页打字输入 ──(TypedInput 命令通道)──▶ 会话调度循环
  └─ 作为独立 TTS task 排到队尾（不打断播放；必要时先收口 ASR 当前句的 task，D24）

翻译模式（`[translate] enabled = true`，D25）：
  ASR/打字文本 → 翻译模式分块（clause：标点离开尾部才切；sentence：只句终）
    └─ 翻译 worker（qwen-mt-flash，单任务保序）→ 译文
         └─ 现有 TTS（目标语言音色）→ 播放
              └─ PipelineEvent::Asr（原文进度）+ PipelineEvent::Translation（译文/句终）
                   └─ 字幕：译文大字行 + 原文小字行，两行各自复制（M2，D26）

关键取舍：

- **低延迟优先**：不等整句识别结束，ASR 文本一稳定就分块送 TTS；语言判定在每句首个 chunk 前完成并冻结。
- **本地只做“轻”处理**：能量 VAD、语言脚本统计、文本分块都是纯本地、无网络、可单测的逻辑；识别与合成全部交给云端。
- **UI 与后台解耦**：后台只通过事件通道汇报，UI 不直接碰音频流或网络任务（D6）。

---

## 2. 代码结构

```text
src/
├── main.rs              GUI 入口：便携路径 → 单实例 → app::run
├── lib.rs               库入口：暴露模块；校验渲染后端 feature
├── app.rs               KaolsApp：启动准备、状态、事件泵、配置写回编排、热重启、退出
├── events.rs            PipelineEvent / SessionState（后台 → GUI 的事件契约）
├── paths.rs             AppPaths（便携路径）+ 内嵌默认配置模板
├── log_sink.rs          log 门面 → 内存环形缓冲 + 按天文件 + panic 钩子
├── single_instance.rs   Windows 命名 Mutex 单实例 + 激活已有窗口
├── config.rs            config.toml 解析、默认值、归一化、校验、URL/路径派生
├── config_edit.rs       toml_edit 定点写回（校验 + .bak）
├── language.rs          语言代码集合、脚本统计、LanguageTracker、instruction 映射
├── audio/
│   ├── device.rs        设备枚举与完整名精确匹配
│   ├── capture.rs       cpal 采集 → 单声道 16k PCM16 → 100ms chunk
│   ├── playback.rs      cpal 播放队列 + 重采样
│   ├── resampler.rs     rubato FFT 重采样封装
│   └── vad.rs           EnergyVad（帧级）+ SpeechGate（发送状态机）
├── client/
│   ├── ws.rs            鉴权建连、Ping 保活、指数退避重连的公共骨架
│   ├── qwen_asr.rs      Qwen-Audio 实时流式识别
│   ├── qwen_tts.rs      Qwen-Audio-TTS 流式合成
│   └── qwen_mt.rs       机器翻译（qwen-mt-flash，OpenAI 兼容 HTTP）
├── pipeline/
│   ├── chunker.rs       流式文本分块（含翻译模式 ChunkPolicy）
│   ├── translator.rs    翻译 worker（单 in-flight 保序，generation 丢弃旧结果）
│   └── session.rs       实时会话编排（ASR → 语言 → 分块 → TTS；打字输入/翻译接入）
├── voice/
│   ├── mod.rs           生效音色解析、样本复刻/缓存注册、语言→(voice_id, 语速) 映射
│   ├── api.rs           百炼声音复刻 HTTP API（不依赖 SDK）
│   ├── cache.rs         voice_cache.json（模型 → 音频 SHA256 → voice_id）
│   ├── editor.rs        音色编辑草稿（config ↔ 草稿 ↔ 写回数据，纯函数）
│   ├── admin.rs         调试 CLI 的纯逻辑（远程音色列出/删除）
│   └── mock_http.rs     单元测试用的手写 HTTP mock
├── gui/
│   ├── toolbar.rs       顶栏：状态灯、VAD 指示器、开始/停止、版本号
│   ├── realtime_tab.rs  实时页：字幕列表（底部嵌入打字输入行）
│   ├── typed_input.rs   实时页底部：多行打字输入框 + Enter/「朗读」提交
│   ├── translate_tab.rs 翻译页：翻译模式开关、目标语言、粒度、显示原文
│   ├── audio_tab.rs     音频页：输入/输出设备
│   ├── tts_tab.rs           语音合成页外壳：页头（保存/放弃）+ 中文方言 + 布局与草稿增删
│   ├── tts_current_voice.rs 语音合成页：顶部「当前音色」选择
│   ├── tts_voice_editor.rs  语音合成页：自定义音色样本编辑/排序/语速
│   ├── settings_tab.rs  设置页：路径、API Key、配置概览
│   ├── log_panel.rs     底部日志面板
│   ├── widgets.rs       通用小部件（错误横幅、键值行、设备下拉）
│   └── mod.rs           GUI 组件入口
├── bin/
│   └── admin.rs         调试 CLI：参数解析 + 终端输出
└── build.rs             把 assets/kaols.ico 写入 exe 资源
```

| 模块 | 职责 | 依赖方向 |
|---|---|---|
| `app` | 应用状态与所有 GUI 操作的编排；唯一同时接触 UI 与后台任务的地方 | 依赖几乎全部下层模块 |
| `gui` | 只负责绘制和把用户操作转成 `KaolsApp` 方法调用 | 依赖 `app` 状态 |
| `pipeline::session` | 一次实时会话的完整生命周期与调度 | 依赖 audio/client/language/chunker/voice |
| `client` | 协议实现；不感知 GUI，通过 channel 上报 | 依赖 `ws` 骨架与 `language` |
| `audio` | 采集/播放/VAD；不感知 GUI | 独立 |
| `voice` | 配置 → 音色解析/注册/缓存 | 依赖 config + 自身 HTTP 客户端 |
| `config`/`config_edit` | 配置模型与安全写回 | 独立（config_edit 依赖 config 校验） |
| `events` | 跨线程数据结构 | 被 app 与 pipeline 共用 |

两条 bin 共用同一个 lib；`admin` 只使用 `config` 与 `voice::{api, admin}`，不启动 GUI、不读写缓存。

---

## 3. 进程与生命周期

### 3.1 启动路径

```text
main()
  ├─ AppPaths::resolve()                 # 以 current_exe() 的父目录为便携根
  ├─ single_instance::acquire(exe)       # 命名 Mutex；已有实例则激活窗口后退出
  └─ app::run(paths)
       ├─ 安装 panic 钩子（尽早，release 无控制台）
       ├─ paths.ensure_default_config() # 缺 config.toml 时写内嵌模板
       ├─ log_sink::cleanup_old_logs()  # 清理过期日志（保留 7 天）
       ├─ log_sink::init()              # 打开按天日志文件、注册 logger
       ├─ 创建多线程 tokio Runtime
       ├─ eframe::run_native(...)       # 1100×720，最小 900×600，wgpu/glow，图标
       └─ KaolsApp::new(...)            # 加载配置、同步界面状态、枚举设备
```

启动阶段的非致命错误不会中止程序：`ensure_default_config`、`log_sink::init` 等失败会记录到 `startup_error`，界面顶部显示黄色横幅。

### 3.2 应用状态

`KaolsApp`（`src/app.rs`）集中保存：

- 路径与运行时：`paths`、`runtime`、`egui_ctx`；
- 会话：`state: SessionState`、`session: Option<PipelineSessionHandle>`、`events_rx`、`running_config_signature`、`restart_pending`；
- 配置：`config: ConfigState`（`Loaded` / `Invalid`）、`api_key_input`、设备列表与当前设备；
- 打字输入：`typed_input`（输入框内容）、`typed_input_error`（行内错误）；提交通过 `PipelineSessionHandle::speak_typed` 走命令通道；
- 音色编辑：`voice_drafts`（未保存草稿）、`selected_voice`、`voice_mode` / `voice_input` / `voice_custom_key`；
- 展示：`subtitles`、`log`、`vad_active`、错误横幅（`startup_error` / `pipeline_error` / `config_write_error` / `crash`）；
- 生命周期：`closing`。

`ConfigState::Invalid` 时程序仍可打开：设置页显示错误链，实时会话无法启动。

### 3.3 主循环

`eframe::App::ui` 每帧依次：

1. `pump_logs()`：把 `log_sink` 累计的日志搬进界面环形缓冲（上限 5000 行）；
2. `pump_events()`：排空 `PipelineEvent`，逐条 `apply_event`；
3. 保存当前 `egui::Context`（热重启时用来请求重绘）；
4. `request_close()`：处理关窗（见 3.5）；
5. 绘制顶栏、底部日志面板（可拖拽高度）、CentralPanel 中的页签。

页签顺序固定为：实时 / 音频 / 语音合成 / 设置。

### 3.4 会话启动

点「开始」后 `app.start_session`：

1. 校验配置为 `Loaded`，否则直接显示错误、不创建会话；
2. 创建无界事件通道、打字输入命令通道与 `PipelineSessionHandle`，记录当前配置指纹，状态置 `Starting`；
3. 只清空「当前字幕句」，历史字幕保留；
4. `runtime.spawn` 执行 `pipeline::session::run(...)`；若返回 `Err`，包装任务补发 `PipelineEvent::Error` + `StateChanged(Failed)`。

`session::run` 内部顺序：

1. 检查 API Key；
2. `voice::resolve_active_voice`（自定义音色可能上传/复刻，耗时不定），发 `VoiceInfo`；
3. 按完整设备名打开输入/输出设备（失败直接结束会话），发 `Devices`；
4. 创建播放引擎（TTS 输出 22050Hz）、启动采集（16kHz）；
5. 启动 ASR、TTS 两个后台任务；
6. 状态置 `Running`；
7. 进入调度循环，直到停止或出错；
8. 收尾（见 3.5）。

### 3.5 停止、失败与关闭

- `PipelineSessionHandle` 持有两个 `Arc<AtomicBool>`，以及一个 GUI → 会话的打字输入无界 `Sender`（D24）：
  - `running`：后台任务的“继续运行”标志，初值 `true`；
  - `stop_requested`：区分“用户主动停止”与“服务异常自行退出”。
- 「停止」：GUI 置 `Stopping`，请求 handle 停止，并立即清空当前字幕句；后台调度循环最多 100ms 内退出。已在命令通道里、尚未开始朗读的打字输入句随会话丢弃（字幕历史保留）。
- 会话收尾顺序：复位 VAD → drop 采集（释放麦克风）→ 关闭 TTS 输入通道 → 最多等待 ASR/TTS 任务 3s → 若非用户主动停止，补发 `Error` → 发 `StateChanged(Idle)`。
- 收到 `Idle`/`Failed` 后 GUI 清空 `session`、配置指纹与 VAD 指示，并清当前字幕句；若 `restart_pending` 且未在关窗，则用新配置自动重启（D20）。
- 关窗：首次收到 close request 时 `CancelClose`、置 `closing`、请求停止会话；下一帧刷新日志并真正 `Close`。这样保证后台任务与音频设备先释放（D13）。

---

## 4. 配置与便携路径

### 4.1 文件布局

`AppPaths` 只从 `current_exe()` 推导：

```text
<exe 目录>/
├── kaols.exe
├── config.toml          # 缺失时由内嵌模板生成
├── config.toml.bak      # 每次 GUI 写回前的完整备份
├── voice_cache.json     # 路径可由 [tts] cache_file 改，默认同目录
├── logs/kaols-YYYY-MM-DD.log
└── crash.log            # panic 时生成
```

样例音频路径可以是相对路径（相对 `config.toml` 所在目录）或绝对路径。主程序不读 `%APPDATA%`、注册表、当前工作目录，也不通过环境变量改变路径（`EGUI_INSPECTION` 只影响调试端口）。

### 4.2 配置模型

`AppConfig`（`src/config.rs`）与 TOML 一一对应，所有结构都启用 `deny_unknown_fields` 且有默认值，因此最小配置只写 `[api] key`。

| 段 | 字段 | 默认值 | 说明 |
|---|---|---|---|
| `[api]` | `key` | 无（必填） | 百炼 API Key，明文保存 |
| | `workspace_host` | `dashscope.aliyuncs.com` | 纯 host；业务空间专属域名可覆盖 |
| `[asr]` | `model` | `qwen-audio-3.1-asr-flash-streaming` | 实时识别模型 |
| | `languages` | `["zh"]` | 有序；首个为默认；整份列表作为 `language_hints`；本地判定也使用完整列表 |
| `[tts]` | `model` | `qwen-audio-3.1-tts-flash` | 合成模型 |
| | `voice` | `qiaoxiaojiao_v3.1` | 系统音色名 / 复刻 voice_id / `custom:<键名>` |
| | `dialect` | `普通话` | 中文方言（官方 20 种 + 普通话）；仅中文任务经 `instruction` 生效（D29） |
| | `cache_file` | `voice_cache.json` | 相对配置文件所在目录 |
| | `custom_voices` | 空 | 键名 → { `name`, `samples[]`（每个样本含 `language` / `audio` / `rate`） } |
| `[audio]` | `input_device` / `output_device` | 空 = 系统默认 | 完整设备名 |
| `[vad]` | `enabled` | `true` | 关闭后全量上传 |
| | `margin_db` | `9.0` | 起音阈值 = 噪声底 + 该值 |
| | `hangover_ms` | `500` | 语音结束拖尾保持 |
| | `tail_ms` | `1000` | 句末补发的静音，用于触发服务端 sentence_end |
| | `debug` | `false` | 打印每个 chunk 的 VAD 决策 |
| `[chunking]` | `words` | `6` | 投机分块阈值 |
| `[translate]` | `enabled` | `false` | 翻译模式开关 |
| | `target` | `en` | 目标语言（16 种 TTS 语言之一） |
| | `model` | `qwen-mt-flash` | 翻译模型 |
| | `granularity` | `clause` | `clause`（标点/句终切 + 安全尾）或 `sentence`（只句终切） |
| | `show_source` | `true` | 字幕是否显示原文行；关掉只显示译文（D26）。不进配置指纹，改后「重新加载配置」即时生效，不重启会话 |

`[tts.custom_voices.<键名>]`：

```toml
[tts.custom_voices.edgar]
name = "异环 - 埃德嘉"      # 可选显示名，默认用键名

[[tts.custom_voices.edgar.samples]]
language = "zh"             # 声音复刻语言代码
audio = "samples/edgar-zh.wav"
rate = 1.0                  # 可选合成语速，默认 1.0，范围 0.5–2.0

[[tts.custom_voices.edgar.samples]]
language = "ja"
audio = "samples/edgar-ja.wav"
rate = 1.0
```

- `samples` 是**有序数组**，第一个是默认声线；
- 每个样本按（TTS 模型, 音频内容 SHA256）单独复刻/缓存；`rate` 不参与该键，改语速不会重新复刻；
- `rate` 属于**样本**（同一音色不同语言的声线可不同），默认 `1.0`，范围 `0.5`–`2.0`；
- `language` 做别名归一（如 `tl` → `fil`），同一音色内归一后不允许重复。

**校验规则**（`AppConfig::validate`）：

- `[api] key` 必填；`workspace_host` 必须是纯 host（无协议、路径、端口、空白，标签合法且长度符合 DNS 规则）；
- ASR/TTS model、TTS voice 非空；
- `[tts] dialect` 必须在 `CHINESE_DIALECTS` 内（官方方言名；归一化只去首尾空白，不接受「粤语」等别名）；
- `[asr] languages` 非空、每个都在 `ASR_LANGUAGES` 内、无重复；归一化时去空白并转小写；
- 自定义音色：引用必须已定义；每个音色至少一个样本；样本 `audio` 非空；语言在 `CLONING_LANGUAGES` 内且归一后不重复；样本 `rate` 必须是有限数且在 `0.5`–`2.0` 内；
- `[chunking] words > 0`；`[vad] margin_db` 必须是有限数；
- `[translate] model` 非空；`enabled = true` 时 `target` 必须在 `TRANSLATION_LANGUAGES`（= 16 种 TTS 语言）内（先做别名归一，如 `tl` → `fil`）；`granularity` 只接受 `clause`/`sentence`。

**派生 URL**：

- WebSocket（ASR/TTS 共用）：`wss://{workspace_host}/api-ws/v1/inference`
- HTTP（声音复刻）：`https://{workspace_host}/api/v1`
- HTTP（翻译，OpenAI 兼容）：`https://{workspace_host}/compatible-mode/v1/chat/completions`

### 4.3 配置写回

`src/config_edit.rs` 提供定点修改函数：

- `set_api_key`、`set_audio_devices`、`set_active_voice`、`set_tts_dialect`、`set_custom_voices`；
- 统一入口 `save(path, original, updated)`：无改动直接返回；否则先用 `AppConfig::from_toml_str` 校验新文本，失败不落盘；成功后把原文件复制为 `config.toml.bak`，再写盘。

`toml_edit` 只改目标字段，保留其它字段、注释与顺序；整体替换 `[tts.custom_voices]` 时该表**内部**注释会丢失（D3）。

### 4.4 配置指纹与热更新

`app::config_signature` 手工拼接一组**顺序稳定**的字段：API Key、域名、ASR 模型/语言、TTS 模型/音色/方言/缓存路径、设备、VAD 参数、分块阈值、翻译开关/目标语言/模型/粒度（`show_source` 只影响渲染，不入指纹），以及按配置名排序的自定义音色与样本（含样本 `rate`）。不能使用 `format!("{config:?}")`（`HashMap` 的 Debug 顺序不稳定）。

写回成功或「重新加载配置」后，`reload_config` 重新读取磁盘并同步界面状态，然后 `maybe_restart_session`：仅当会话处于 `Starting`/`Running` 且指纹变化时，停旧会话、等 `Idle` 后用新配置重启（D20）。

---

## 5. 线程与事件模型

| 执行体 | 职责 |
|---|---|
| 主线程（eframe） | 绘制、处理输入、配置写回、启动/停止会话、排空事件 |
| tokio 多线程 Runtime | 会话编排任务、ASR 任务、TTS 任务、声音复刻网络请求 |
| cpal 采集回调线程 | 混音、重采样、按块 `try_send` 到采集通道 |
| cpal 播放回调线程 | 从播放队列取样本填缓冲区，不足补静音 |

事件流：

```text
后台任务 ──(PipelineEvent, unbounded)──▶ KaolsApp::pump_events
      └── ctx.request_repaint() ──▶ egui 下一帧

实时页输入框 ──(TypedInput, unbounded)──▶ session 调度循环（biased 优先）
```

- `PipelineEvent` 字段以 `src/events.rs` 为唯一事实源；事件不携带 egui 类型。
- `TypedSentence`：会话接受一句打字输入、已交给 TTS 排队后发给界面（进字幕历史）。
- `Translation`：翻译模式下的译文（可能随 ASR 修订多次到达），`language` 固定为目标语言；界面用它累积译文行，句终时与原文行一起进历史（M2，D26）。
- 翻译模式下 `Asr` 事件语义：`sent_bytes` = 已送翻译的源文长度、`is_sentence_end` 恒为 `false`、`language` = 源语言；句终与当前行收口一律由 `Translation` 负责（D26）。
- GUI → 后台唯一命令通道是打字输入通道；会话调度循环用 `biased` select 优先处理它，避免按下回车后又把当前 ASR 句的 chunk 送进 TTS（D24）。
- ASR→会话、会话→TTS、采集→ASR 使用有界 channel（容量 100）；采集端用 `try_send`，满则丢弃，避免阻塞实时回调。
- 会话状态机：`Idle → Starting → Running → Stopping → Idle`；失败路径为 `Starting/Running → Failed`。GUI 以 `Idle`/`Failed` 作为“会话已结束”的信号。
- 后台服务非用户主动退出时，会补发 `Error` 再发 `Idle`：错误条保留，状态回到可再次启动的空闲态。

---

## 6. ASR 客户端

### 6.1 WebSocket 公共骨架（`client/ws.rs`）

- 建连：`Authorization: Bearer <key>` + `X-DashScope-DataInspection: enable`，10s 超时。
- 连接级保活：每 20s 发一帧 WebSocket Ping（inference 连接约 60s 无帧会被服务端关闭；Ping 只能保连接，不能保 run-task 任务）。
- 断线重连：指数退避 500ms → 8s；会话连续存活 ≥10s 后重置退避；`run_with_reconnect` 统一驱动。
- 每次重连前调用 `SessionState::before_reconnect` 清理瞬态。

### 6.2 ASR 协议

- 建连后发 `run-task`：`task_group=audio`、`task=asr`、`function=recognition`、配置的模型；参数 `format=pcm`、`sample_rate=16000`、`vad_model=near_meeting_16k`、`heartbeat=true`、`language_hints=<完整 [asr] languages>`。
- 等待 `task-started`；`task-failed` 视为握手失败并触发重连。
- 双向流式：一边把（可经 VAD 门控的）PCM 二进制帧发上去，一边读 JSON 事件。
- `result-generated` 中取 `payload.output.sentence`：忽略 `heartbeat=true` 与空文本，其余转成 `AsrEvent { text, is_sentence_end, sentence_id }` 送入会话调度。
- 用户停止/音频通道关闭时发 `finish-task`；服务端提前返回 `task-finished` 视为异常断线并重连。
- 重连前：丢弃积压音频、向下游发“空文本 + 句终”的重置信号、重置 VAD 门控的瞬态（保留噪声底估计）。

---

## 7. TTS 客户端

### 7.1 协议

- **每句一个 run-task**：`task_group=audio`、`task=tts`、`function=SpeechSynthesizer`、配置的 TTS 模型。
- 每句首 chunk 固定参数：`format=pcm`、`sample_rate=22050`、`voice`、`language_hints=[tts_language_hint(语言)]`、`volume=50`、`rate`（取本句所用样本的语速；内置音色/普通 voice_id 固定 `1.0`，回退默认样本时用该样本的 `rate`）、`pitch=1.0`；`instruction` 由 `language::tts_instruction(本句语言, [tts] dialect)` 决定：中文用方言指令（普通话不附加），其余语种用内置语种指令。同一 task 的后续 chunk 复用这些参数。
- 文本通过 `continue-task` 逐段追加；句终发 `finish-task`；服务端返回二进制 PCM 帧，直接推入播放队列。
- 单条 WS 连接跨句子长期复用（省 TLS/握手）；空闲与任务中都定期发 Ping 保活。

### 7.2 重连与句级重试

- 建连、握手或 `task-failed` 失败 → 返回“断线”，由公共骨架退避重连。
- 当前句首个 chunk 保存在 `pending_chunk` 中：启动失败时重连后重试同一句，避免整句漏合成；连续失败 3 次则丢弃该句，避免一句坏数据卡死后续字幕。
- 断线时把重采样器内部余量 flush 到播放队列（不丢弃已排队音频），避免采样边界错位。
- 会话在 ASR 句终但没有新文本时，会补发“空文本 + 句终”请求：TTS 只发 `finish-task` 结束当前 task，不追加空 continue-task；若 TTS 此时空闲则直接忽略。

### 7.3 播放

TTS 返回的 22050Hz PCM16 进入 `AudioPlaybackHandle` 的队列；输出设备采样率不同时用 rubato FFT 重采样。播放回调每帧从队列取一个样本复制到所有声道，队列为空时补静音。每句 task 结束后 `flush()` 把重采样器内部余量补齐。

### 7.4 打字输入与任务边界（D24）

打字输入句不走流式分块，而是整句一条 `continue-task` + `finish-task`，由会话在**任务边界**处插入：

- 不取消 task、不清空播放队列：已排队/正在播放的音频照常播完，打字输入句的音频排在其后（FIFO）。
- 若当前 ASR 句的 TTS task 还开着（会话侧 `tts_task_language` 非空），会话先发「空文本 + 句终」：TTS 只发 `finish-task` 正常收口已收到的文本，然后新请求自然成为下一个 task。
- 若 ASR 当前句尚未句终，会话先按「已送 TTS 前缀」把字幕句收尾，再丢弃该句剩余事件到服务端句终，期间不再送 TTS；因此被丢弃部分的文本不会被合成。
- 播放层与 TTS 客户端因此无需感知“打字输入”：它们只看到正常的 task 序列。

---

## 8. 语言判定

官方 ASR 只接受 `language_hints` 输入，**不返回识别到的语种**，因此需要在 ASR 文本上做本地判定。

- `ScriptStats::count` 统计汉字、假名、谚文、拉丁、西里尔、阿拉伯、泰文、希腊、天城文等脚本数量（忽略空白/数字/标点）。
- `LanguageTracker` 规则：
  1. 出现假名 → `ja`；出现谚文 → `ko`（强证据）；
  2. 汉字多于拉丁且当前是 `ja`：无假名且汉字 ≥6 时切 `zh`，否则保持 `ja`（纯汉字短句粘性）；
  3. 汉字多于拉丁且当前不是 `ja` → `zh`；
  4. 拉丁字母 ≥2 且多于汉字 → `allowed` 列表里第一个拉丁语言（没有则 `en`）；
  5. 其它脚本按映射：西里尔→`ru`、阿拉伯→`ar`、泰文→`th`、希腊→`el`、天城文→`hi`；
  6. 文本文字数 <2 或证据不足 → 保持当前语言；
  7. 判定结果不在 `allowed` 内 → 回退 `allowed[0]`。
- 判定生命周期：**每个 TTS task 在首个 chunk 送出前判定一次并冻结**，同一 task 内后续 ASR 修订不再改语言；空文本 + 句终重置。
- **打字输入句与 ASR 共用同一个 `LanguageTracker`**：判定时继承当前对话的粘性语言，并把结果回写 `current`，因此打字输入会影响后续句子的语言判定（D24）。收口当前 ASR 句时只清句文本/任务态，不重建 tracker。
- 合成参数：`language_hints` 用 `tts_language_hint`（复刻语言集合内直接用，否则 `other`）；`tts_instruction(语言, dialect)` 为中文提供方言指令（`[tts] dialect`，普通话不附加），为 ja/ko/fr/de/ru/es/pt/it/th/vi/id/ms/fil/ar 提供内置语种指令，en 不附加（D29）。
- 这套判定与复刻样本语种无关；样本缺失目标语言时回退第一个样本（连同其 `rate`，D11）。

---

## 9. 流式文本分块

`pipeline/chunker.rs` 的输入是 ASR 当前句的**完整文本**（会持续修订）与 `is_sentence_end`，输出一到多个待合成切片。

1. **对齐**：把当前完整文本与已发射文本对齐，找出未消费起点。对齐支持 ASR 修订：
   - 标点插入/删除会被跳过；
   - 实质字符不一致时，在前后 3 个字符窗口内重新同步，否则按“单字替换”推进；
   - 已发射 ≥6 字符但当前文本更短且完全对不上时，视为外部重置。
2. **句终**：直接把未消费的剩余文本 trim 后作为一个 `is_sentence_end` 切片，然后重置。
3. **句中标点**：默认策略遇到 `, . ? ! ; :` 或中文标点即切。
4. **无标点时（默认策略）**：
   - 含 CJK：字符数达到 `chunking.words × 2 + guard_tail`（默认 12+3）后切，避免把中英混排的英文单词截断；
   - 无 CJK：空格分词数达到 `chunking.words + guard` 后按词边界切。
5. **策略开关（`ChunkPolicy`）**：默认 `word_cut = true`、`punctuation_cut = true`、`punctuation_tail_guard = 0`，行为与第 1–4 条一致。
   - `translation_clause()`：关闭词数兜底，只按标点/句终切；标点切点距当前文本末尾不足 3 个字符时**先不切**，等 ASR 文本延展后再切（译文不可撤回，避免送未收敛尾部）；`is_sentence_end` 不受该限制。
   - `translation_sentence()`：只在句终切，句内不送翻译。
   - 安全尾只作用于标点分支，且仅翻译模式开启；现有 TTS 路径行为与测试不变。
6. `emitted_byte_len(full_text)` 返回已送入 TTS 的字节前缀，供字幕分色；该前缀一定是字符边界。

---

## 10. 音频采集、播放与 VAD

### 10.1 采集（`audio/capture.rs`）

- 使用设备默认输入配置，支持 `f32` / `i16` 两种采样格式；其它格式明确报错。
- 多声道混成单声道，经 rubato FFT 重采样到 16kHz，累积到 1600 个样本（100ms）后转 PCM16 LE 发送；通道满时丢弃（实时场景可接受）。
- `AudioCapture` 持有 cpal `Stream`，drop 即停止采集。

### 10.2 播放（`audio/playback.rs`）

- 使用设备默认输出配置，支持 `f32` / `i16`；内部队列保存 i16 单声道样本。
- 若 TTS 采样率（22050）与设备采样率不同，`push_pcm16` 时重采样；回调只负责从队列取数、复制到各声道，队列空补 0。
- `flush()` 在每句结束时补齐重采样器内部余量。

### 10.3 VAD 与发送门控（`audio/vad.rs`）

目的：只上传语音段，降低 ASR 费用，同时避免服务端任务因长时间没有音频而超时。

**能量 VAD（逐 20ms 帧）**

- 计算 RMS dBFS；自适应噪声底（下降快、上浮慢）；
- 起音阈值 = 噪声底 + `margin_db`，并有绝对下限；连续 `onset_frames` 帧超阈值才算语音；
- 维持阈值比起音阈值低 3dB（迟滞），`hangover_ms` 内仍视为语音；
- 启动前 300ms 只标定噪声底、不判定。

**发送门控（`SpeechGate`）**

- 语音中：发送当前 chunk；起音瞬间补发 `pre_roll_ms`（默认 300ms）历史音频，避免切掉首字；
- 语音刚结束：继续补 `tail_ms`（默认 1000ms）静音，帮助服务端 VAD 吐出 `sentence_end`（服务端约需 800ms 静音）；
- 完全空闲：每 `keepalive_ms`（默认 10s）补一小段静音，避免 run-task 任务约 23s 无音频被服务端失败；
- 每个 chunk 更新共享 `active_flag`（顶栏 VAD 指示器）。
- 配置 `[vad] enabled=false` 时跳过门控，全量上传；重连时 `reset_transient()` 清空 pre-roll/tail/keepalive 状态，但保留噪声底估计。

---

## 11. 自定义音色与缓存

### 11.1 配置与展示

- 音色结构见 4.2；显示名 = `name`（缺省为配置键名）。
- 合成时按本句语言找样本：先归一语言，命中该语言样本则用其 voice_id 与该样本的 `rate`；否则回退第一个样本（并用其 `rate`，D11）。

### 11.2 GUI 编辑草稿（`voice/editor.rs`）

- `drafts_from_config`：配置 → `VoiceDraft`（键名、显示名、有序样本，样本含语言、音频路径与 `rate`），按显示名/键名排序；
- 界面只改草稿，直到点「保存」才写回；每个样本一行「语速」滑条（0.5–2.0，步长 0.05）与「重置」；
- `drafts_to_entries` 校验：键名非空且唯一、每个音色至少一个样本、语言合法且归一后唯一、音频路径非空、`rate` 为有限数且在 0.5–2.0 内；
- 音频路径策略：选中的文件在 exe 目录内 → 存相对路径；目录外 → 存绝对路径并在界面提示可能破坏便携性；
- 保存前还会检查每个样本音频文件存在；写回走 `config_edit::set_custom_voices`（整体替换该表）。

### 11.3 运行期解析与注册（`voice/mod.rs`）

会话启动时 `resolve_active_voice`：

1. `[tts] voice` 不是 `custom:` → 直接作为内置音色/voice_id 使用，不联网；
2. 是 `custom:<键名>`：
   - 逐样本解析音频路径、读取文件；读不到时尝试用缓存里“同路径”的记录兜底；
   - 读文件前按元数据检查 ≤10MB；WAV 若可解析，时长 <5s 报错、>60s 警告；>20s 时提示只有前 20s 参与复刻（D30）；
   - 按音频内容 SHA256 分组（同一音频多语言只复刻一次）；
   - 每组先查缓存并调用 `query_voice` 校验：可用则复用；云端不存在则重新复刻；网络错误则标记“未校验”并先沿用缓存；
   - 未命中的音频上传 → 取临时下载 URL → `create_voice`（带 `max_prompt_audio_length = 20.0`，D30）→ 轮询到就绪（最多 60s）→ 写缓存；注册任务并行执行，部分失败时保留已成功记录并返回错误；
   - 汇总「语言（归一后）→ (voice_id, rate)」，默认语言取第一个样本的语言；`rate` 不参与复刻/缓存键。

百炼音色前缀由配置键名自动生成：仅保留 ASCII 字母/数字、转小写，最多 10 字符；多语言样本追加语言后缀（如 `edgarzh`）。

### 11.4 `voice_cache.json`

```json
{
  "voices": {
    "qwen-audio-3.1-tts-flash": {
      "<音频 SHA256>": {
        "voice_id": "...",
        "language": "zh",
        "name": "埃德嘉",
        "audio_path": "samples/edgar-zh.wav",
        "created_at": 1730000000
      }
    }
  }
}
```

- 两层结构：TTS 模型 → 音频内容 SHA256 → 记录；同一音频在不同模型下分别缓存，切换模型不覆盖其它模型记录。
- 保存是“临时文件 + rename”的原子写；解析失败时把旧文件备份为 `voice_cache.json.bak` 并以空缓存重建。
- 本机状态，不提交、不参与同步；孤儿条目无害，v1 不提供清理入口（D11）。

### 11.5 `admin` 调试 CLI

- 命令：`list`、`delete --voice-id <id>...`、`delete-all [-y]`；默认读工作目录 `./config.toml`，`-c` 可覆盖。
- 只做远程音色管理：不读写 `voice_cache.json`、不 enroll、不做 prefix 过滤。
- 列表要求完整分页（否则报错，避免“半截列表”被误认为全部）；删除区分“已删除”与“本来就不存在”；批量删除给出成功/已不存在/失败三类汇总。
- 逻辑在 `voice/admin.rs`，用 mock HTTP 单测；bin 只负责参数与终端输出。

---

## 12. GUI 结构

- 顶栏：应用名、会话状态灯、VAD 指示器、开始/停止、版本号。
- 底部日志面板：可拖拽高度（默认 180，最小 60），等宽字体、按级别着色，只渲染最近 500 行。
- 中央页签：

| 页签 | 职责 | 关键行为 |
|---|---|---|
| 实时 | 字幕列表 + 打字输入 | 当前句在最上、历史句新→旧；语言列 + 进度分色 + 单句复制（D16）；翻译模式下每条字幕为「译文大字行 + 原文小字行」，两行各自复制，`show_source=false` 只渲染译文行（D26）；底部多行输入框（默认 1 行、约 4 行封顶后内部滚动）Shift+Enter 换行、Enter/「朗读」排队朗读打字输入（D24），仅运行中可提交，上限 500 字符 |
| 翻译 | 翻译模式与目标语言 | 开关 + 16 种目标语言下拉（中文名 + 代码）+ 粒度二选一 + 显示原文开关（D25/D26）；改值即走 `config_edit::set_translate` 写回并重载：`show_source` 只影响渲染、不重启，其余字段触发 D20 热重启；`model` 只读展示 |
| 音频 | 输入/输出设备 | 设备下拉保存完整设备名；改动后立即写回并重载（可能触发 D20 热重启）；「刷新设备」「恢复系统默认」；配置值不在设备列表时橙色警告 |
| 语音合成 | 当前音色 + 中文方言 + 自定义音色编辑器 | 顶部 Radio 现有/自定义音色 + 中文方言下拉（选项来自 `CHINESE_DIALECTS`，默认普通话）；Tab 级保存/放弃更改（一次写回 `[tts] voice`/`dialect`/`custom_voices`）；左侧列表、右侧详情（显示名、键名、样本增删/拖拽排序、语言下拉、`rfd` 选音频、每个样本的语速滑条与「重置」）。页面拆为 `tts_tab.rs`（外壳/方言/列表编排）+ `tts_current_voice.rs` + `tts_voice_editor.rs` |
| 设置 | 便携路径、API Key、配置概览 | API Key 密码框 + 保存；重新加载配置；打开程序目录；只读展示域名/模型/语言/音色/方言；配置无效时显示错误链 |

- 错误展示：启动横幅（黄）、管线错误（红，可清除）、写回错误（页内红字）、崩溃信息（红）。
- 语音合成页的“脏”状态 = 音色草稿 / 当前音色 / 中文方言任一与配置不一致；保存/放弃按钮只在脏时可用；`● 未保存` 为琥珀色提示。
- 语言列宽度、字号、颜色衰减等实现细节见 `gui/realtime_tab.rs` 的常量；字幕状态由 `app::SubtitleState` 的纯函数维护（普通模式 `apply_asr` / `push_typed`；翻译模式 `apply_source` / `apply_translation`），并有单测覆盖。
- 打字输入行在 `gui/typed_input.rs`：多行输入，Shift+Enter 换行、Enter 提交并保持焦点；Windows 的 IME 确认候选由 egui-winit 的 `VK_PROCESSKEY` 过滤，不会误提交。

---

## 13. 测试与验证

- 测试全部是模块内 `#[cfg(test)]` / `#[tokio::test]`，当前共 **179** 个（以 `cargo test` 输出为准）。
- 覆盖重点：配置解析/校验、`config_edit` 写回、音色草稿、字幕 reducer、语言判定、分块（含翻译模式策略与安全尾）、ASR/TTS 协议解析、翻译客户端（协议/重试/错误）、缓存、VAD、单实例命名等纯逻辑。
- Mock 方式：WebSocket 测试在测试内起本地 `TcpListener` 手写握手/报文；HTTP 测试使用 `voice/mock_http.rs`；**不访问真实百炼网络**。
- UI 验证：`EGUI_INSPECTION=1` + egui_mcp 做真实点击与截图；没有 UI 自动化测试（不使用 egui_kittest，D14）。
- 静态截图：glow 变体 + `EFRAME_SCREENSHOT_TO`（wgpu 不支持该调试特性）。
- 发布前人工冒烟：开始/停止、配置保存、双开单实例、release 启动、便携目录行为。
- 仓库没有 CI，门禁靠本地手动执行（具体命令与提交规范见 [CONTRIBUTING.md](../CONTRIBUTING.md)）。

---

## 14. 已知限制与技术债

- **仅 Windows 10/11 x64**；无跨平台、无安装器、无自动更新。
- `[tts.custom_voices]` 整体重写会丢失该表内部注释（表外保留，D3）。
- 设备热插拔不自动感知，只能点「刷新设备」；配置的设备名对不上时不会回退默认设备（D12）。
- `voice_cache.json` 的孤儿条目（配置删了音色、缓存仍在）不主动清理；云端音色被删除时靠下次会话启动的校验自动重建（D11/D19）。
- 跨机便携（拷贝整个目录到另一台机器）没有自动化测试，发布前需人工冒烟。
- 中文方言的效果取决于音色是否支持方言指令：复刻音色支持官方 20 种方言，`qwen-audio-3.1-tts-flash` 的 4 个多语种/方言系统音色仅支持 8 种，其余系统音色可能只支持普通话。程序不做音色能力探测，不支持的组合可能仍按普通话合成（D29）。
- `AudioPlaybackHandle::interrupt` 与 `AudioCapture::stop` 当前没有调用方（预留接口）；播放/采集的生命周期由 drop 控制。
- 打字输入句是**排队**语义：当前排队/播放的音频全部播完后才轮到它；界面在提交时就把该句加入字幕历史，可能与实际发声时间有延迟。
- 若提交打字输入时 ASR 当前句尚未句终，该句剩余部分会被丢弃、不会补读（字幕只保留已送 TTS 的前缀，D24）；极端情况下用户持续说话期间打字输入会暂停该句后续合成。
- 打字输入句上限 500 字符；TTS 连续启动失败 3 次时该句按现有机制丢弃（只写日志，不弹错误条）。
- 翻译模式（M3）提供独立「翻译」页：开关、目标语言、粒度、显示原文都可图形化修改并热更新；`model` 仍只读（手改配置后「重新加载配置」生效）。字幕双语（原文行 + 译文行，两行各自复制），`show_source = false` 时只显示译文（D25/D26）。
- 翻译是逐块 HTTP 请求：clause 模式的实际延迟、端到端 1s 目标、16 语种 TTS 自然度与碎句译文质量尚未做真实语料评估。
- 已送 TTS 的译文不可撤回；翻译失败的片段不发声，只按 D8 显示错误条。
- 没有 CI；UI 与真实音频链路的回归依赖人工验证。

---

## 15. 翻译模式（M1–M3）

链路：现有 ASR/打字输入 → 本地语言判定与分块 → **翻译 worker**（`client/qwen_mt.rs` + `pipeline/translator.rs`）→ 译文 → 现有 TTS/播放与字幕。

- **开关与目标语言**：`[translate] enabled/target/model/granularity/show_source`（D25）。M3 起由「翻译」页图形化修改（`enabled/target/granularity/show_source` 可写，`model` 只读并展示）；`enabled = false` 时整条链路与旧行为完全一致。
- **翻译接口**：`POST https://{workspace_host}/compatible-mode/v1/chat/completions`，`model = qwen-mt-flash`；`messages` 单条 user 文本；`translation_options = { source_lang, target_lang }` **必填**（缺失时模型会退化成普通对话）。源/目标语言直接用 ISO 代码，`source_lang = "auto"` 可兜底；目标语言限 16 种 TTS 语言。
- **worker**：独立 tokio 任务，单 in-flight 顺序处理（`TranslationJob`/`TranslationDone` 带 `generation` 与 `seq`），主循环不 await HTTP；打字输入会 bump `generation` 并丢弃旧句在途译文。
- **句终屏障**：源文句终时若还有译文在途，等 `pending` 归零后再发「空文本 + 句终」收口 TTS task；打字输入立即收口当前 task 并丢弃未返回译文。译文全部失败时也照样补发句终事件，保证当前字幕行被收口（D26）。
- **屏障期间的 ASR 缓存**：句终屏障未完成时，新到达的 ASR 事件先进 `asr_backlog`，等旧句句终 `Translation` 发出、状态复位后再按序补处理——否则下一句源文会先于旧句句终译文落到界面，出现「一句变两句、原文译文错位」（D28）。
- **分块策略**：`clause`（默认）= 标点/句终切 + 安全尾；`sentence` = 只句终切（见第 9 节）。
- **字幕/事件**：翻译模式下 `PipelineEvent::Asr` 只描述原文进度（`sent_bytes` = 已送翻译源文长度、`is_sentence_end` 恒 false、`language` = 源语言）；`PipelineEvent::Translation{text, sent_bytes, is_sentence_end, language, sentence_id}` 负责译文与句终（`language` 固定为目标语言）。`SubtitleState` 用 `apply_source` / `apply_translation` 维护双语当前行与历史，同一 `sentence_id` 去重（D26）。
- **双语渲染**：实时页一条字幕 = 译文大字行（24pt）+ 原文小字弱色行（15pt），两行各有语言列、各自复制；`show_source = false` 只渲染译文行（D26）。
- **失败处理**：翻译失败发 `PipelineEvent::Error`（D8），该片段不送 TTS，后续片段继续。
- **验证**：`cargo test` 覆盖 qwen_mt 协议/重试/错误、chunker 安全尾、config 校验与语言表、字幕双语 reducer；运行期用真实接口 + 打字输入验证 zh→en / zh→ja 双语字幕与源/译文各自复制，`show_source` 热加载即时生效，ASR 语音路径用环境语音间接验证；M3 用「翻译」页验证开关/目标语言/粒度改动触发 D20 热重启、`show_source` 不重启、配置写回保留 `model`。
