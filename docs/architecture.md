# 架构与模块设计

面向项目开发与贡献者，描述 Bivox 当前实现的结构、数据流、关键算法与边界。

- “为什么这样设计”见 [decisions.md](./decisions.md)；
- 用户可见行为以 `README.md` 为准；
- 本文只描述当前代码，不记录历史版本或迁移过程。改动模块边界、数据流或算法时，请同步更新本文；新的长期设计取舍写到 [decisions.md](./decisions.md)。

---

## 1. 系统概览

Bivox 是一个 Windows 10/11 x64 桌面应用：采集麦克风音频，实时送云端 ASR，按标点/语义把识别文本切成小段，再流式送 TTS 合成并播放，做到“边说边出字幕、边合成”。

- 纯 GUI（egui/eframe），便携模式：配置、缓存、日志都在 exe 同目录（D4）；
- 内部全异步：tokio runtime 跑后台任务，eframe 占主线程（D5）；
- 四个二进制：`bivox.exe`（GUI 主程序）与 `admin.exe` / `vad_debug.exe` / `loopback_debug.exe` 三个调试 CLI（都不随发布包分发，D19）；
- 外部依赖：阿里云百炼 DashScope 的 Qwen-Audio 实时 ASR/TTS WebSocket 接口、声音复刻 HTTP 接口，以及 ASR 预编译热词管理 HTTP 接口（D31）；
- 翻译模式（`[self] translate`，D25）：ASR/打字文本先经 `qwen-mt-flash` 翻译成目标语言，再用目标语言音色合成，字幕以「译文 + 原文」双语呈现（M2，D26；独立 GUI 翻译页是 M3）；
- VRChat OSC 聊天框（`[vrchat.osc]`，D33/D42）：会话事件额外喂给独立后台任务，按固定 1 秒文本间隔、typing 跳变、可配置的历史句窗口（默认保留最近 30 秒）更新本机 `127.0.0.1:9000` 的 ChatBox；
- 他人线路（`[others]`，D36/D40）：独立 tokio 任务用进程环回采集 VRChat 的音频 → 本地 VAD → 第二条 ASR，文本只进时间线上屏（不发 OSC、不做 TTS）；进程不可达时按退避自动重连，不影响会话；
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

会话启动（D31）：
  配置 [asr] hotwords → 查 cache.json → create/update/query 百炼预编译热词列表
    └─ vocabulary_id 写入 cache.json → 作为 ASR run-task 的 parameters.vocabulary_id

翻译模式（`[self] translate = true`，D25）：
  ASR/打字文本 → 翻译模式分块（clause：标点离开尾部才切；sentence：只句终）
    └─ 翻译 worker（qwen-mt-flash，单任务保序）→ 译文
         └─ 现有 TTS（目标语言音色）→ 播放
              └─ PipelineEvent::Asr（原文进度）+ PipelineEvent::Translation（译文/句终）
                   └─ 字幕：译文大字行 + 原文小字行，两行各自复制（M2，D26）

VRChat OSC 聊天框（D33）：
  pipeline::session::emit（事件克隆一份）→ ChatboxService 后台任务
    └─ 自己的 Timeline + 1 秒文本节流 + typing 跳变 + 按 retention_secs 主动清空（默认 30 秒）
         └─ rosc 编码 → UDP 127.0.0.1:{vrchat.osc.port}（/chatbox/input、/chatbox/typing）
  顶栏「清除 VRChat 聊天框」──(Command::ForceClear)──▶ 同一任务：清镜像 Timeline + 立即空串（D44）

关键取舍：

- **低延迟优先**：不等整句识别结束，ASR 文本一稳定就分块送 TTS；语言判定在每句首个 chunk 前完成并冻结。
- **本地只做“轻”处理**：Silero VAD（ONNX 推理，模型随 exe 分发）、语言脚本统计、文本分块都是纯本地、无网络的逻辑；识别与合成全部交给云端。
- **UI 与后台解耦**：后台只通过事件通道汇报，UI 不直接碰音频流或网络任务（D6）。

---

## 2. 代码结构

```text
src/
├── main.rs              GUI 入口：便携路径 → 单实例 → app::run
├── lib.rs               库入口：暴露模块；校验渲染后端 feature
├── app/                 应用主体：mod.rs（状态、事件泵、配置写回编排、热重启、退出）+ drafts.rs（各页签草稿）
├── events.rs            PipelineEvent / SessionState / Speaker（后台 → GUI 的事件契约）
├── paths.rs             AppPaths（便携路径）+ 空配置初始化
├── log_sink.rs          log 门面 → 内存环形缓冲 + 按天文件 + panic 钩子
├── single_instance.rs   Windows 命名 Mutex 单实例 + 激活已有窗口
├── config.rs            config.toml 解析、默认值、归一化、校验、URL/路径派生
├── config_edit.rs       配置读写：`update` 单一入口、最小化序列化、校验、`.bak` 备份、原子替换
├── cache.rs             本地共享缓存 cache.json（音色 voice_id + ASR vocabulary_id）
├── chatbox.rs           VRChat 聊天框文本合成：当前句+历史句、逐句截断、双语软借用（D33/D42）
├── osc.rs               VRChat OSC 后台服务：1 秒文本节流、typing、保留时长清空、测试消息、手动清除（D33/D42/D44）
├── timeline.rs         Timeline：ASR/翻译/打字事件的纯归约（GUI 与 OSC 共用；含句终时间戳）
├── http.rs              百炼 HTTP JSON 接口的公共错误类型与响应解析
├── language.rs          语言代码集合、脚本统计、LanguageTracker、instruction 映射
├── asr/
│   ├── mod.rs           预编译热词运行期注册（创建/校验/更新/轮询/降级）
│   ├── api.rs           百炼热词管理 HTTP API（speech-biasing 的 5 个 action）
│   └── admin.rs         调试 CLI 的热词列表列出/删除纯逻辑
├── audio/
│   ├── device.rs        设备枚举与完整名精确匹配
│   ├── capture.rs       cpal 采集 → 单声道 16k PCM16 → 100ms chunk
│   ├── meter.rs         PCM16 RMS → dBFS → 0..1；GUI 电平平滑（D43）
│   ├── process_loopback.rs WASAPI 进程环回：按 PID 采某进程树的音频（D36）
│   ├── process_capture.rs  进程环回采集线程：PID 发现、100ms 攒块、墙钟补静音（D36）
│   ├── playback.rs      cpal 播放队列 + 重采样
│   ├── resampler.rs     rubato FFT 重采样封装
│   ├── silero.rs        Silero VAD 模型加载 + 帧级语音概率（ONNX Runtime）
│   ├── vad.rs           VAD 门控状态机（起音/保持/发送决策）+ 帧检测器 trait
│   └── vad_thread.rs    独立 VAD 线程（采集 chunk → 门控 → ASR）
├── client/
│   ├── ws.rs            鉴权建连、Ping 保活、指数退避重连的公共骨架
│   ├── qwen_asr.rs      Qwen-Audio 实时流式识别
│   ├── qwen_tts.rs      Qwen-Audio-TTS 流式合成
│   └── qwen_mt.rs       机器翻译（qwen-mt-flash，OpenAI 兼容 HTTP）
├── pipeline/
│   ├── asr_line.rs      识别线路接线：chunk → VAD 线程 → ASR 任务 → AsrEvent（两条线路共用）
│   ├── self_line.rs     我方调度：ASR → 语言判定 → 分块 → 翻译/TTS/字幕 + 打字输入
│   ├── others_line.rs   他人线路：进程环回采集 → ASR → 只上屏；自带退避重连（D40）
│   ├── chunker.rs       流式文本分块（含翻译模式 ChunkPolicy）
│   ├── translator.rs    翻译 worker（单 in-flight 保序，generation 丢弃旧结果）
│   └── session.rs       会话编排：建连、设备、音色、翻译 worker 与生命周期收尾
├── voice/
│   ├── mod.rs           生效音色解析、样本复刻/缓存注册、语言→(voice_id, 语速) 映射
│   ├── api.rs           百炼声音复刻 HTTP API（不依赖 SDK）
│   ├── editor.rs        音色编辑草稿（config ↔ 草稿 ↔ 写回数据，纯函数）
│   ├── admin.rs         调试 CLI 的纯逻辑（远程音色列出/删除）
│   └── mock_http.rs     单元测试用的手写 HTTP mock
├── gui/
│   ├── toolbar.rs       顶栏布局：麦克风/他人开关与电平条、状态、开始/停止、清除聊天框、版本号（D43/D44）
│   ├── theme.rs         通用配色（错误/告警/正常/提示），各页签共用
│   ├── meter.rs         顶栏图标开关 + 20 段电平条（VAD 合并配色，D43）
│   ├── banners.rs       顶部横幅：崩溃 / 启动 / 配置 / 他人线路状态 / 运行告警 / 管线错误
│   ├── realtime_tab.rs  实时页：气泡时间线的列表（滚动、空状态），单条气泡在 `bubble`
│   ├── bubble.rs        单条消息气泡：左右分侧、宽度贴合、原文/译文两行、外侧语言代码（D16/D26/D39）
│   ├── typed_input.rs   实时页底部：多行打字输入框 + Enter/「朗读」提交
│   ├── audio_tab.rs     音频页：输入/输出设备
│   ├── self_tab.rs      「自己」页：`[self]` 麦克风/语言列表/翻译/合成开关（D38）
│   ├── others_tab.rs    「他人」页：`[others]` 开关/语言列表/翻译（D38）
│   ├── language_list.rs 有序语言列表编辑器（两页共用，D38）
│   ├── drag_reorder.rs  拖动排序（「≡」手柄 + 插入位置指示线；语言列表与音色样本共用）
│   ├── asr_tab.rs       语音识别页：共用的识别模型与预编译热词
│   ├── tts_tab.rs           语音合成页外壳：页头（保存/放弃）+ 中文方言 + 布局与草稿增删
│   ├── tts_current_voice.rs 语音合成页：顶部「当前音色」选择
│   ├── tts_voice_editor.rs  语音合成页：自定义音色样本编辑/排序/语速
│   ├── translate_tab.rs 翻译页：共用的粒度、显示原文与翻译模型
│   ├── vrchat_tab.rs    VRChat 页：采集进程、OSC 开关/端口/显示模式/历史句/保留时长、发送测试消息（D33/D38/D42）
│   ├── settings_tab.rs  设置页：API Key、高级设置、整页保存
│   ├── log_panel.rs     底部日志面板
│   ├── widgets.rs       通用小部件（错误横幅、页签保存栏、错误行、键值行、设备下拉）
│   └── mod.rs           GUI 组件入口
├── bin/
│   ├── admin.rs         调试 CLI：参数解析 + 终端输出
│   ├── vad_debug.rs     VAD 调参工具（麦克风/录音重放）
│   └── loopback_debug.rs 进程环回采集调试：逐秒电平 + WAV（D36）
└── build.rs             把 assets/bivox.ico 写入 exe 资源
```

| 模块 | 职责 | 依赖方向 |
|---|---|---|
| `app` | 应用状态与所有 GUI 操作的编排；唯一同时接触 UI 与后台任务的地方 | 依赖几乎全部下层模块 |
| `gui` | 只负责绘制和把用户操作转成 `BivoxApp` 方法调用 | 依赖 `app` 状态 |
| `pipeline::session` | 一次实时会话的编排：建连、设备、音色、翻译 worker、生命周期与收尾 | 依赖 audio/client/voice/asr + `asr_line` / `self_line` |
| `pipeline::asr_line` | 「chunk → VAD → ASR」的共用接线，产出 `AsrEvent` 流 | 依赖 audio/client |
| `pipeline::self_line` | 我方文本流转：语言判定、分块、翻译、TTS 边界、打字输入 | 依赖 client/chunker/translator/voice/language |
| `pipeline::others_line` | 他人线路：进程环回采集 → 识别 → 只上屏，自带退避重连 | 依赖 audio/process_capture + `asr_line` + language |
| `client` | 协议实现；不感知 GUI，通过 channel 上报 | 依赖 `ws` 骨架与 `language` |
| `audio` | 采集/播放/VAD；不感知 GUI | 独立 |
| `voice` | 配置 → 音色解析/注册/缓存 | 依赖 config + `cache` + 自身 HTTP 客户端 |
| `asr` | 预编译热词注册与管理 API；不感知 GUI | 依赖 config + `cache` + `http` |
| `timeline` | 对话时间线归约（两条线路的消息、翻译、打字、去重） | 独立，纯逻辑 |
| `chatbox` | VRChat 聊天框文本合成与截断 | 依赖 config + `timeline` |
| `osc` | VRChat OSC 后台发送、节流、typing、清空 | 依赖 config + `timeline` + `chatbox` + rosc |
| `config`/`config_edit` | 配置模型与安全写回 | 独立（config_edit 依赖 config 校验） |
| `events` | 跨线程数据结构 | 被 app 与 pipeline 共用 |

四个 bin 共用同一个 lib。`admin` 只使用 `config`、`voice::{api, admin}`、`asr::{api, admin}`，不启动 GUI、不读写缓存；`vad_debug` / `loopback_debug` 只用 `audio` 与 `config`（VAD 调参、进程环回采集），不联网、不需要 API Key。

---

## 3. 进程与生命周期

### 3.1 启动路径

```text
main()
  ├─ AppPaths::resolve()                 # 以 current_exe() 的父目录为便携根
  ├─ single_instance::acquire(exe)       # 命名 Mutex；已有实例则激活窗口后退出
  └─ app::run(paths)
       ├─ 安装 panic 钩子（尽早，release 无控制台）
       ├─ log_sink::cleanup_old_logs()   # 清理过期日志（保留 7 天）
       ├─ log_sink::init()               # 打开按天日志文件、注册 logger
       ├─ paths.ensure_default_config()  # 缺 config.toml 时写空文件；失败弹窗退出
       ├─ AppConfig::load(...)           # 解析/归一化/校验；失败弹窗退出，不创建主界面
       ├─ 创建多线程 tokio Runtime
       ├─ eframe::run_native(...)        # 1100×720，最小 900×600，wgpu/glow，图标
       └─ BivoxApp::new(...)             # 接收已校验配置、同步界面状态、枚举设备
```

日志初始化失败是非致命错误：记录到 `startup_error`，界面顶部显示黄色横幅。配置文件创建/读取/解析/校验失败是致命错误：弹原生错误窗后退出（D35）。

### 3.2 应用状态

`BivoxApp`（`src/app/mod.rs`）集中保存：

- 路径与运行时：`paths`、`runtime`、`egui_ctx`；
- 会话：`state: SessionState`、`session: Option<PipelineSessionHandle>`、`events_rx`、`running_config_signature`、`restart_pending`；
- VRChat：`chatbox: ChatboxService`（后台 OSC 任务句柄；不参与配置指纹）；
- 配置：`config: AppConfig`（启动时已校验，始终有效）、`config_reload_error`、`api_key_notice`、设备列表 `input_devices` / `output_devices`；
- 页签草稿：`drafts: Drafts`（`src/app/drafts.rs`）——每页一个结构化草稿（`settings` / `asr` / `tts` / `local` / `others` / `vad` + `translate_model` / `vrchat_process`），各自带 `from_config`（从配置生成）、`is_dirty`（与配置比较）、`apply_to`（写回配置）。逐字段的 `sync_*` / `*_dirty` 不再散在 `BivoxApp` 上；
- 页签错误：`errors: BTreeMap<Tab, String>`（页内红字，`error_for(tab)` 取用）与 `config_write_error`（不归属任何页签的写回失败，走顶部横幅）；
- 打字输入：`typed_input`（输入框内容）、`typed_input_error`（行内错误）；提交通过 `PipelineSessionHandle::speak_typed` 走命令通道；
- 音色编辑的界面选中态：`selected_voice`、`voice_list_scroll_to_end`（只是 UI 状态，不属于配置草稿）；
- 展示：`timeline`、`log`、`vad_active` / `vad_active_others`、顶栏两条电平的平滑值 `mic_level` / `others_level`（`audio::meter::MeterSmoother`）、其余横幅字段（`startup_error` / `config_reload_error` / `pipeline_warning` / `pipeline_error` / `crash`，绘制在 `gui/banners.rs`）；
- 生命周期：`closing`。

`[api] key = None` 不算配置无效：GUI 正常打开；点「开始」时跳转设置页并显示 `api_key_notice`。真正非法的配置在启动阶段就弹窗退出，不会进入 `BivoxApp`。

### 3.3 主循环

`eframe::App::ui` 每帧依次：

1. `pump_logs()`：把 `log_sink` 累计的日志搬进界面环形缓冲（上限 5000 行）；
2. `pump_events()`：排空 `PipelineEvent`，逐条 `apply_event`；
3. `pump_meters()`：读取 handle 的两条电平，做快起慢落平滑；运行中按 ~30fps 请求重绘；
4. 保存当前 `egui::Context`（热重启时用来请求重绘）；
5. `request_close()`：处理关窗（见 3.5）；
6. 绘制顶栏、底部日志面板（可拖拽高度）、CentralPanel 中的页签。

页签顺序固定为：实时 / 音频 / 自己 / 他人 / 语音识别 / 语音合成 / VRChat / 翻译 / 设置（D38）。

### 3.4 会话启动

点「开始」后 `app.start_session`：

1. 检查 API Key 是否已配置；未配置则跳转「设置」页并提示，不创建会话；
2. 创建无界事件通道、打字输入命令通道与 `PipelineSessionHandle`，记录当前配置指纹，状态置 `Starting`；
3. 只清空「当前字幕句」，历史字幕保留；
4. `runtime.spawn` 执行 `pipeline::session::run(...)`；若返回 `Err`，包装任务补发 `PipelineEvent::Error` + `StateChanged(Failed)`。

`session::run` 内部顺序（能力开关决定建什么，D37）：

1. 检查 API Key；
2. `[self] tts = true` 时 `voice::resolve_active_voice`（自定义音色可能上传/复刻，耗时不定），发 `VoiceInfo`；否则完全跳过音色解析；
3. `asr::resolve_vocabulary`（D31）：按 `[asr] hotwords` 创建/校验/更新百炼预编译热词列表，
   写回 `vocabulary_id`；失败只发 `Warning` 并降级为不带热词；
4. `[self] tts = true` 时按完整名打开输出设备并创建播放引擎（TTS 输出 22050Hz）；
5. `[self] tts = true` 时启动 TTS 任务；需要翻译时建 `QwenMtClient` + 翻译 worker，得到 `TranslationRuntime`；
6. 创建本地 ASR 事件稳定通道；若 handle 的麦克风期望状态为开，打开输入设备并启动首条「采集 → 电平 tap → VAD → ASR」线路，失败仍按原行为结束会话；发 `Devices`（未启用的设备留空）；
7. spawn 两个线路监督者：
   - 麦克风监督者按 handle 的 `watch` 期望状态反复重建/停止上述线路；
   - 他人监督者按 `watch` 期望状态 spawn/停止 `pipeline::others_line::run`；进程不可达由该线路自己退避重连；
   然后置 `Running`；
8. 把 `local_rx` 交给 `pipeline::self_line::run`（我方调度循环；`voice` / `tx_tts` 缺失时对应环节变成空操作），直到停止或出错；
9. 收尾（见 3.5）。

> 麦克风/他人开关的即时切换走监督者的 `watch`，不触发 D20 全量热重启（D43）；只有顶栏开关之外的配置变更才走 D20。

### 3.5 停止、失败与关闭

- `PipelineSessionHandle` 持有两条线路的期望状态 `watch`、实时电平 `AtomicU32`、VAD/降级标志，以及一个 GUI → 会话的打字输入无界 `Sender`（D24/D43）：
  - `running`：后台任务的“继续运行”标志，初值 `true`；
  - `stop_requested`：区分“用户主动停止”与“服务异常自行退出”；
  - `mic_enabled` / `others_enabled`：顶栏开关的期望状态，监督者按它启停对应线路；
  - `mic_level` / `others_level`：顶栏电平条读取的共享值。
- 「停止」：GUI 置 `Stopping`，请求 handle 停止，并立即清空当前字幕句；后台调度循环最多 100ms 内退出。已在命令通道里、尚未开始朗读的打字输入句随会话丢弃（字幕历史保留）。
- 会话收尾顺序：复位 VAD/电平 → 取消两个监督者 → 麦克风监督者 drop 采集（释放麦克风）→ 等 VAD 线程/ASR 任务退出（每条线路 ≤3s）→ 他人监督者等 `others_line` 退出 → 关闭 TTS 输入通道 → 最多等待 TTS/翻译任务 3s → 若非用户主动停止，补发 `Error` → 发 `StateChanged(Idle)`。
- 收到 `Idle`/`Failed` 后 GUI 清空 `session`、配置指纹、VAD/电平并清当前字幕句；若 `restart_pending` 且未在关窗，则用新配置自动重启（D20）。
- 运行中切换顶栏开关：写回配置 → 更新 `running_config_signature`（避免误触发 D20）→ 通知 handle 的 `watch`；监督者只启停该线路并释放对应资源。
- 关窗：首次收到 close request 时 `CancelClose`、置 `closing`、请求停止会话；下一帧刷新日志并真正 `Close`。这样保证后台任务与音频设备先释放（D13）。

---

## 4. 配置与便携路径

### 4.1 文件布局

`AppPaths` 只从 `current_exe()` 推导：

```text
<exe 目录>/
├── bivox.exe
├── config.toml          # 缺失时生成空文件（字段走内置默认值）
├── config.toml.bak      # 每次 GUI 写回前的完整备份
├── cache.json           # 本地共享缓存：音色 voice_id + ASR 预编译热词 vocabulary_id
│                        # 路径可由配置修改
├── logs/bivox-YYYY-MM-DD.log
└── crash.log            # panic 时生成
```

样例音频路径可以是相对路径（相对 `config.toml` 所在目录）或绝对路径。主程序不读 `%APPDATA%`、注册表、当前工作目录，也不通过环境变量改变路径（`EGUI_INSPECTION` 只影响调试端口）。

### 4.2 配置模型与派生值

`AppConfig`（`src/config.rs`）与 TOML 一一对应，所有结构启用 `deny_unknown_fields` 且有默认值；空文件是合法的默认配置。**字段语义、允许值/范围与默认值统一以 `src/config.rs` 的字段注释、`Default` 实现和默认常量为唯一来源**，本节不再维护字段表。

字段级校验与跨字段规则集中在 `AppConfig::validate`；需要特别注意：

- `[api] key` 缺省合法（未配置），显式空字符串非法；其余字段的显式空字符串同样非法。
- `[tts] voice = "custom:<名字>"` 必须引用已定义的 `custom_voices` 条目。
- 每个自定义音色至少一个样本；样本语言归一后不能重复。
- 语言列表按线路各一份：`[self] languages`（我方，含打字输入的语言判定）与 `[others] languages`（对方）；`[asr]` 只保留两条线共用的模型与热词（D37）。
- `[self] target` / `[others] target` 只在各自 `translate = true` 时校验是否为可合成语言；`[vrchat] process` 不能为空。
- `[cache] file`、`[audio]` 设备名、样本 `audio` 等路径/名称不能为空。

**派生 URL**：

- WebSocket（ASR/TTS 共用）：`wss://{workspace_host}/api-ws/v1/inference`
- HTTP（声音复刻 / ASR 热词管理共用基址）：`https://{workspace_host}/api/v1`
- HTTP（翻译，OpenAI 兼容）：`https://{workspace_host}/compatible-mode/v1/chat/completions`

### 4.3 配置写回

- 统一入口 `config_edit::update(path, |config| { … })`：读磁盘原文 → 解析为带 `base_dir` 的 `AppConfig` → 类型化修改 → 校验磁盘原配置与新文本 → 备份 `config.toml.bak` → **原子替换**（先写 `config.toml.tmp` 再 rename）→ 返回写好的配置。调用方拿返回值直接更新内存状态，不重新读盘（单字段保存因此只读一次文件）。
- `AppConfig::to_toml_string` 先归一化/校验副本，再只输出与默认值不同的字段：结构体用 `derive(Serialize)` 声明字段，序列化后递归删掉与 `AppConfig::default()` 相同的项与变空的子表，并把浮点收敛成 `f32` 的最短十进制（`0.42` 而不是 `0.41999998688697815`）。因此字段只需在结构体上声明一次，**不会出现「新增字段忘了写进序列化器、写回时静默丢值」**；`custom_voices` 另按键名排序（`HashMap` 顺序不稳定）。
- `config.toml` 由 GUI 维护，不再保留注释、字段顺序或用户手写格式。设置页整页保存时 API Key 输入框留空 = 删除 `[api] key`（`None`），不写空字符串。

### 4.4 配置指纹与热更新

`app::config_signature` 手工拼接一组**顺序稳定**的字段：API Key、域名、ASR 模型/热词、我方线路（麦克风开关/语言列表/翻译开关/译文语言/TTS 开关）、对方线路（开关/语言列表/翻译开关/译文语言）、`[vrchat] process`、TTS 模型/音色/方言、缓存路径、设备、VAD 参数、分块阈值、翻译模型/粒度（`show_source` 只影响渲染，不入指纹），以及按配置名排序的自定义音色与样本（含样本 `rate`）。**`[vrchat.osc]` 不入指纹**：由 `ChatboxService` 的 watch 实时读取，改设置不重启（D33）。不能使用 `format!("{config:?}")`（`HashMap` 的 Debug 顺序不稳定）。

写回成功或「重新加载配置」后，`reload_config` 重新读取磁盘并同步界面状态，然后 `maybe_restart_session`：仅当会话处于 `Starting`/`Running` 且指纹变化时，停旧会话、等 `Idle` 后用新配置重启（D20）。重新加载失败时保留当前内存配置并显示 `config_reload_error`；写回会重新校验磁盘文件，非法时拒绝覆盖（D35）。

---

## 5. 线程与事件模型

| 执行体 | 职责 |
|---|---|
| 主线程（eframe） | 绘制、处理输入、配置写回、启动/停止会话、排空事件、平滑电平 |
| tokio 多线程 Runtime | 会话编排任务、麦克风/他人线路监督者、ASR 任务、TTS 任务、声音复刻网络请求 |
| cpal 采集回调线程 | 混音、重采样、按块 `try_send` 到采集通道 |
| cpal 播放回调线程 | 从播放队列取样本填缓冲区，不足补静音 |
| 线路监督者任务 | 按 `watch` 期望状态启停麦克风线路或 `others_line`；释放设备/线程 |

事件流：

```text
后台任务 ──(PipelineEvent, unbounded)──▶ BivoxApp::pump_events
      └── ctx.request_repaint() ──▶ egui 下一帧

pipeline::self_line ──(经 session::emit 克隆, unbounded)──▶ ChatboxService（D33）
      └── 独立任务：我方投影（`speaker_view(Speaker::Local)`）→ 1 秒文本节流 / typing / 清空

实时页输入框 ──(TypedInput, unbounded)──▶ session 调度循环（biased 优先）

顶栏开关 ──(watch<bool>)──▶ 线路监督者 ──▶ start/stop 麦克风线路 / others_line
顶栏电平 ◀──(AtomicU32)── asr_line 电平 tap（VAD 之前）
```

- `PipelineEvent` 字段以 `src/events.rs` 为唯一事实源；事件不携带 egui 类型。
- `Asr` / `Translation` / `VadActive` 带 `speaker`（`Local` 我方 / `Others` 他人，D39）：两条线路共用同一套事件与时间线，靠来源区分；OSC 与 TTS 只消费 `Local`。
- `OthersStatus`：他人线路的状态跳变（`Connecting` / `Capturing` / `Waiting` + `retry_in`，D40），界面据此显示带倒计时的状态横幅。
- `LineError`：某条线路无法启动（目前是麦克风设备打开失败，D43）；界面回退顶栏开关并显示黄色告警，不结束会话。
- 顶栏两条电平不走事件通道：`asr_line` 的电平 tap 写 handle 上的 `AtomicU32`，GUI 每帧读取并平滑；只在运行中请求 ~30fps 重绘。
- 顶栏两个开关走 `watch<bool>`，监督者按最新期望值启停对应线路；这不算 D20 配置热更新。
- 会话的 `emit` 同时向 GUI 与 OSC 两个无界通道发送事件克隆；OSC 任务不依赖 GUI 重绘（D33）。
- `Warning`：用户可感知但不影响会话继续的降级（例如预编译热词注册失败，D31），界面显示黄色「运行告警」条，可手动清除。
- `TypedSentence`：会话接受一句打字输入、已交给 TTS 排队后发给界面（进字幕历史）。
- `Translation`：翻译模式下的译文（可能随 ASR 修订多次到达），`language` 固定为目标语言；界面用它累积译文行，句终时与原文行一起进历史（M2，D26）。
- 翻译模式下 `Asr` 事件语义：`sent_bytes` = 已送翻译的源文长度、`is_sentence_end` 恒为 `false`、`language` = 源语言；句终与当前行收口一律由 `Translation` 负责（D26）。
- GUI → 后台：打字输入走无界命令通道，会话调度循环用 `biased` select 优先处理它，避免按下回车后又把当前 ASR 句的 chunk 送进 TTS（D24）；顶栏两个开关走 `watch<bool>`，由线路监督者按最新期望值启停（D43）。
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

- 建连后发 `run-task`：`task_group=audio`、`task=asr`、`function=recognition`、配置的模型；参数 `format=pcm`、`sample_rate=16000`、`vad_model=near_meeting_16k`、`heartbeat=true`、`language_hints=<完整 [asr] languages>`；配置了预编译热词时再加 `vocabulary_id=<D31 解析出的列表 ID>`。
- 等待 `task-started`；`task-failed` 视为握手失败并触发重连。
- 双向流式：一边把（可经 VAD 门控的）PCM 二进制帧发上去，一边读 JSON 事件。
- `result-generated` 中取 `payload.output.sentence`：忽略 `heartbeat=true` 与空文本，其余转成 `AsrEvent { text, is_sentence_end, sentence_id }` 送入会话调度。
- 用户停止/音频通道关闭时发 `finish-task`；服务端提前返回 `task-finished` 视为异常断线并重连。
- 重连前：丢弃积压音频、向下游发“空文本 + 句终”的重置信号，并置位 VAD 线程的 `vad_reset` 标志（模型 state/起音/尾部在下一块音频前复位）。

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
- 多声道混成单声道，经 rubato FFT 重采样到 16kHz，累积到 ≥1600 个样本后**整块**转 PCM16 LE 发送（名义 100ms；实际块长由声卡回调粒度决定，本机两种设备实测 102–107ms）；通道满时丢弃并计数（`AudioCapture::dropped_chunks`，供排障工具查看）。
- `AudioCapture` 持有 cpal `Stream`，drop 即停止采集。

### 10.2 播放（`audio/playback.rs`）

- 使用设备默认输出配置，支持 `f32` / `i16`；内部队列保存 i16 单声道样本。
- 若 TTS 采样率（22050）与设备采样率不同，`push_pcm16` 时重采样；回调只负责从队列取数、复制到各声道，队列空补 0。
- `flush()` 在每句结束时补齐重采样器内部余量。

### 10.3 VAD 与发送门控（`audio/silero.rs` / `audio/vad.rs` / `audio/vad_thread.rs`）

目的：只上传语音段，降低 ASR 费用，同时避免服务端任务因长时间没有音频而超时（D34）。

**Silero VAD（16k / 32ms 帧）**

- 模型为官方 `silero_vad_16k_op15.onnx`（MIT，1.23MiB），编译期 `include_bytes!` 内嵌进 exe，运行时由 ONNX Runtime 从内存加载；模型来源/版本/SHA256 见 `assets/silero/README.md`。
- 每帧恰好 512 采样（32ms）；输入 `input[1,576]`（64 采样 context + 512 新帧）、`state[2,1,128]`、`sr`；输出该帧语音概率与下一帧 state。
- 采集块名义 100ms（实际 100–107ms）：门控内部按 512 分帧，跨 chunk 保留不足一帧的采样余量（以 1600 为例 3×512 + 64）。
- 实测（release，单线程）：单帧约 0.13ms；会话启动加载会话约 36ms。单帧推理不在 cpal 实时回调里执行。

**发送门控（`SpeechGate`）**

- 起音：连续 2 帧概率 ≥ `threshold`（默认值见 `VadConfig::default`，约 64ms 去抖）；
- 保持：概率 ≥ `threshold - 0.15`（负阈值迟滞）；
- 结束：低于负阈值持续 `min_silence_ms`（默认值见 `VadConfig::default`）；
- 语音中：发送当前 chunk；起音瞬间补发 `pre_roll_ms`（内部固定 300ms）历史音频，避免切掉首字（采集块不一定整除毫秒，按**整块向上取整**保留，实际补发可能略多于配置值）；
- 语音刚结束：继续补 `tail_ms`（默认值见 `VadConfig::default`）静音，帮助服务端 VAD 吐出 `sentence_end`（服务端约需 800ms 静音）；
- 完全空闲：每 `keepalive_ms`（默认 10s）补一小段静音，避免 run-task 任务约 23s 无音频被服务端失败；
- 每个 chunk 更新共享 `active_flag`（顶栏电平条的彩色/灰阶判断）。
- `asr_line` 在 VAD 之前对每个 chunk 做电平 tap（RMS → dBFS → 0..1），写入顶栏共享 `AtomicU32`（D43）；电平 tap 在 tokio 任务里，不阻塞 cpal 回调。
- `[vad] enabled = false` 时跳过门控，全量上传（字段语义见 `src/config.rs`）。

**独立 VAD 线程**

- `vad_thread` 用 `std::thread("silero-vad")` 持有检测器与门控：收采集原始 chunk，输出应上传的 chunk 给 ASR 任务；cpal 实时回调只做重采样/攒块，ASR 任务只发 WebSocket。
- 断线重连：ASR 置位 `vad_reset`，VAD 线程在下一块音频前 `reset_transient()`（模型 state、起音/拖尾、pre-roll、keepalive）。
- 推理失败：门控降级为全量上传（不再丢音频），会话轮询到 `vad_degraded` 后发一次 `PipelineEvent::Warning`（GUI 告警横幅，指示器灰）；停止后重新开始会话可重试。
- 收尾：先 drop 采集释放麦克风，再等 VAD 线程退出（≤3s），最后等 ASR/TTS 任务。

### 10.4 进程环回采集（对方语音，D36）

`audio/process_loopback.rs` + `audio/process_capture.rs` 提供「采某个进程树渲染出来的音频」的采集原语，音源不是输入设备，而是另一个进程的输出。

- **激活**：`ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, IAudioClient::IID, PROPVARIANT{VT_BLOB: AUDIOCLIENT_ACTIVATION_PARAMS})`，参数为 `TargetProcessId` + `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`；完成回调只做一次同步唤醒，激活结果在采集线程里同步取出（超时 8 秒）。
- **格式**：`Initialize` 时固定 16 kHz / 单声道 / PCM16（`AUTOCONVERTPCM` + `SRC_DEFAULT_QUALITY`），声道混合与重采样交给 WASAPI。
- **线程与 RAII**：`ProcessLoopback` 持有 COM 初始化守卫与事件句柄，必须在创建它的线程上 drop（字段顺序保证接口指针先释放、再 `CoUninitialize`）；采集线程是 `std::thread("process-loopback")`。
- **PID 与存活**：ToolHelp 快照按 exe 文件名匹配（同名多个取第一个并记 warning，空名视为未配置）；每 1 秒用同一份快照判断目标进程是否还在，避免 `OpenProcess` 跨完整性级别被拒导致误判。
- **攒块**：队列攒够 3200 字节（100 ms）用 `try_send` 送进容量 10 的有界 channel；下游积压超过 1 秒、`AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`、目标进程退出都返回错误，由上层决定是否重连。
- **补静音**：WASAPI 安静期可能长时间不送包，采集层按墙钟补零块，保证下游每秒收到约 10 块（ASR run-task 约 23 秒没有音频会被服务端判失败）；落后超过 1 秒时重置时间基准，不追补旧时间。
- 采集线程成功激活后先发 `ready`，退出时发 `done`（`Ok` = 正常停止，`Err` = 需要重连）。

---

## 11. 自定义音色与缓存

### 11.1 配置与展示

- 音色字段见 `src/config.rs`；显示名 = `name`（缺省为配置键名）。
- 合成时按本句语言找样本：先归一语言，命中该语言样本则用其 voice_id 与该样本的 `rate`；否则回退第一个样本（并用其 `rate`，D11）。

### 11.2 GUI 编辑草稿（`voice/editor.rs`）

- `drafts_from_config`：配置 → `VoiceDraft`（键名、显示名、有序样本，样本含语言、音频路径与 `rate`），按显示名/键名排序；
- 界面只改草稿，直到点「保存」才写回；每个样本一行「语速」滑条与「重置」；
- 语音识别页的草稿与保存规则见 D32；热词不是草稿的一部分，保存后由下一次会话启动同步（D31）。
- `drafts_to_entries` 只检查草稿列表自己的约束（键名非空/不重复），条目字段的校验（语言归一、音频、`rate`）直接调用 `config::validate_custom_voice`——与 `AppConfig::validate` 是**同一份代码**，避免「配置合法但 GUI 存不回去」；保存前还会检查音频文件存在；写回时整体替换 `AppConfig.tts.custom_voices`。
- 音频路径策略：选中的文件在 exe 目录内 → 存相对路径；目录外 → 存绝对路径并在界面提示可能破坏便携性。

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

### 11.4 `cache.json`

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
  },
  "asr_hotwords": {
    "qwen-audio-3.1-asr-flash-streaming": {
      "vocabulary_id": "vocab-bivox-...",
      "hotwords": ["原神"],
      "weight": 50,
      "updated_at": 1730000000
    }
  }
}
```

- `voices` 两层结构：TTS 模型 → 音频内容 SHA256 → 记录；同一音频在不同模型下分别缓存，切换模型不覆盖其它模型记录（D11）。
- `asr_hotwords`：ASR 模型 → 预编译热词列表 ID + 词条快照 + **权重快照** + 最后更新时间；用于下次会话复用、校验与 5 分钟更新限速（D31）。权重变化（例如默认值从 4 改成 50）也会触发一次列表更新。
- 保存是“临时文件 + rename”的原子写；解析失败时把旧文件备份为 `cache.json.bak` 并以空缓存重建。
- 本机状态，不提交、不参与同步；孤儿条目无害，v1 不提供清理入口（D11）。

### 11.5 `admin` 调试 CLI

- 音色命令：`voice list`、`voice delete --voice-id <id>...`、`voice delete-all [-y]`；默认读工作目录 `./config.toml`，`-c` 可覆盖。
- 热词命令：`vocab list`、`vocab delete --vocabulary-id <id>... [-y]`。
- 只做远程管理：不读写 `cache.json`、不 enroll、不创建/更新热词列表、不做 prefix 过滤。
- 列表要求完整分页（否则报错，避免“半截列表”被误认为全部；热词列表官方响应没有 total，靠短页判断）；删除区分“已删除”与“本来就不存在”；批量删除给出成功/已不存在/失败三类汇总。
- 热词不提供 delete-all：无法区分是否为其它应用创建的列表，避免误删（D31）。
- 逻辑在 `voice/admin.rs` 与 `asr/admin.rs`，用 mock HTTP 单测；bin 只负责参数与终端输出。

---

## 12. GUI 结构

- 顶栏：麦克风/他人侦听图标开关 + 20 段电平条（VAD 合并配色，D43）、会话状态灯、开始/停止、清除 VRChat 聊天框（需启用 OSC，D44）、版本号；不再显示应用名与副标题。线路关闭时电平条整体隐藏但保留 100×14 位置，避免布局跳动。
- 底部日志面板：可拖拽高度（默认 180，最小 60），等宽字体、按级别着色，只渲染最近 500 行。
- 中央页签：

| 页签 | 职责 | 关键行为 |
|---|---|---|
| 实时 | 气泡时间线 + 打字输入 | **两条未完成消息永远钉在最上面**，定稿后才沉到历史区；历史区新→旧；**我方气泡靠右、他人气泡靠左**，宽度贴合内容（上限约 72%）；翻译模式下气泡内是「原文小字弱色行 + 主行大字（20pt）」，**原文语言**代码画在气泡外侧（他人气泡右侧 / 我方气泡左侧，翻译模式也标原文语种），`show_source=false` 只渲染主行（D26/D39）；当前句按「已送入 / 尚未送入」分色；所有文本（原文与译文）都可拖动选中并用 Ctrl+C 复制，界面不提供复制按钮（D16）；底部多行输入框（默认 1 行、约 4 行封顶后内部滚动）Shift+Enter 换行、Enter/「朗读」排队朗读打字输入（D24），仅运行中可提交，上限 500 字符 |
| 音频 | 输入/输出设备 + VAD | 设备下拉保存完整设备名，改动后立即写回并重载（可能触发 D20 热重启）；「刷新设备」「恢复系统默认」；配置值不在设备列表时橙色警告。VAD 参数（启用/阈值/静音/tail/debug）用「保存 VAD 设置」写回，运行中触发 D20 热重启 |
| 自己 | 我方线路 | `[self]` 的麦克风开关、语言列表、翻译开关与目标语言、合成开关；麦克风开关切换即写回并启停线路（D43），其余字段 Tab 级保存/放弃更改（D38）；运行中保存其余字段触发 D20 热重启 |
| 他人 | 他人线路 | `[others]` 的侦听开关、语言列表、翻译开关与目标语言；侦听开关切换即写回并启停线路（D43），其余字段 Tab 级保存/放弃更改（D38）；音源在「VRChat」页设置 |
| 语音识别 | 共用的识别模型 + 预编译热词 | 模型输入框（可恢复默认）；热词多行文本框（一行一个，权重固定 50 超级热词、最多 50 条）；Tab 级保存/放弃更改（一次保存模型/热词，D32/D38）；运行中保存触发 D20 热重启；热词在下一次会话启动时自动同步到百炼（D31） |
| 语音合成 | 合成模型 + 当前音色 + 中文方言 + 自定义音色编辑器 | 顶部合成模型输入框（可恢复默认），Radio 现有/自定义音色 + 中文方言下拉（选项来自 `CHINESE_DIALECTS`）；Tab 级保存/放弃更改；左侧列表、右侧详情（显示名、键名、样本增删/拖拽排序、语言下拉、`rfd` 选音频、每个样本的语速滑条与「重置」）。页面拆为 `tts_tab.rs`（外壳/模型/方言/列表编排）+ `tts_current_voice.rs` + `tts_voice_editor.rs` |
| 翻译 | 共用的翻译设置 | 粒度二选一 + 显示原文开关 + 翻译模型输入/应用（D25/D26/D38）；`show_source` 只影响渲染、不重启，粒度变化触发 D20 热重启 |
| VRChat | 采集进程 + OSC 聊天框 | `[vrchat] process` 文本框 + 「应用」（D36/D38：进指纹，运行中应用会重启会话）；OSC 启用开关、发送端口（1–65535）、三种显示模式、发送测试消息（D33；改值即保存、不重启）；测试消息立即发送、不走 1 秒文本节流 |
| 设置 | 百炼 + 高级设置 | 整页「保存 / 放弃更改」：百炼（API Key 留空 = 清除、接入域名）与高级设置（缓存文件、分块词数）一次写回；底部「重新加载配置」「打开程序目录」；未配置 Key 时显示弱提示 |

- 错误展示：启动横幅（黄）、配置重新加载失败（红）、他人线路状态横幅（黄，实时状态、带倒计时、不可手动关闭，D40）、运行告警（黄，可清除，D8/D31）、管线错误（红，可清除）、写回错误（页内红字）、崩溃信息（红）。配置启动失败不会进入主界面，而是原生错误弹窗后退出（D35）。
- 语音合成页的“脏”状态 = 合成模型 / 音色草稿 / 当前音色 / 中文方言任一与配置不一致；保存/放弃按钮只在脏时可用；`● 未保存` 为琥珀色提示。
- 语音识别页的“脏”状态 = 模型 / 归一化后的热词列表任一与配置不一致（空行/重复项不算改动）。
- 「自己」「他人」页的“脏”状态 = 该页所有草稿字段与 `[self]` / `[others]` 任一不一致；保存写成一次类型化写回（D38）。
- 语言列宽度、字号、颜色衰减等实现细节见 `gui/realtime_tab.rs` 的常量；对话时间线由 `timeline::Timeline` 的纯函数维护（`apply_asr` / `apply_translation` / `push_typed`，每条消息带来源 `Speaker`），GUI 与 VRChat OSC 任务共用同一份归约逻辑，并有单测覆盖。
- 打字输入行在 `gui/typed_input.rs`：多行输入，Shift+Enter 换行、Enter 提交并保持焦点；Windows 的 IME 确认候选由 egui-winit 的 `VK_PROCESSKEY` 过滤，不会误提交。

---

## 13. 测试与验证

- 测试全部是模块内 `#[cfg(test)]` / `#[tokio::test]`；数量以 `cargo test` 输出为准（`bin` 下的调试工具另有少量单测）。
- 覆盖重点：配置解析/校验、`config_edit` 写回、音色草稿、字幕 reducer、语言判定、分块（含翻译模式策略与安全尾）、ASR/TTS 协议解析、翻译客户端（协议/重试/错误）、缓存、预编译热词注册与管理 API、VAD、PCM 电平映射与平滑、`Timeline::drop_open`、进程环回配速与进程发现、单实例命名、VRChat 聊天框合成/截断/OSC 编码/1 秒节流等纯逻辑。
- Mock 方式：WebSocket 测试在测试内起本地 `TcpListener` 手写握手/报文；HTTP 测试使用 `voice/mock_http.rs`；**不访问真实百炼网络**。
- UI 验证：`EGUI_INSPECTION=1` + egui_mcp 做真实点击与截图；顶栏开关需要额外验证「运行中只启停该线路、不重启会话」「页签 checkbox 同步」「900px 最小宽度」；没有 UI 自动化测试（不使用 egui_kittest，D14）。
- 静态截图：glow 变体 + `EFRAME_SCREENSHOT_TO`（wgpu 不支持该调试特性）。
- 发布前人工冒烟：开始/停止、配置保存、双开单实例、release 启动、便携目录行为。
- CI（`.github/workflows/ci.yml`）在 windows-latest 上跑 `cargo fmt --check` / `cargo build --bins` / `cargo test` / `cargo clippy --all-targets -- -D warnings`，工具链钉在 1.99.1（clippy 的默认 lint 集合随版本变化，门禁需要可复现）；提交前仍须在本机执行同一组门禁（具体命令与提交规范见 [CONTRIBUTING.md](../CONTRIBUTING.md)）。

---

## 14. 已知限制与技术债

- **仅 Windows 10/11 x64**；无跨平台、无安装器、无自动更新。
- 配置写回是整份类型化重写：`config.toml` 中用户手写的注释和字段顺序都不会保留（D3）。
- 设备热插拔不自动感知，只能点「刷新设备」；配置的设备名对不上时不会回退默认设备（D12）。
- `cache.json` 的孤儿条目不主动清理：配置删了音色/热词记录仍保留；云端音色被删除时靠下次会话启动的校验自动重建（D11/D19）；云端热词列表被删除时自动重建（D31）。
- 预编译热词仅华北2（北京）/新加坡地域、**仅主业务空间**支持；子业务空间或其它地域会创建失败并降级为不带热词识别（黄色告警）。
- 每账号最多 50 个预编译热词列表；固定使用超级热词权重 50，因此每个列表最多 50 条。换 ASR 模型会新建列表，旧列表需用 `admin vocab delete` 清理；每次列表创建/更新后生效时间受百炼部署延迟影响，更新还受「两次间隔 ≥5 分钟」限制（D31）。
- 跨机便携（拷贝整个目录到另一台机器）没有自动化测试，发布前需人工冒烟。
- 中文方言的效果取决于音色是否支持方言指令：复刻音色支持官方 20 种方言，`qwen-audio-3.1-tts-flash` 的 4 个多语种/方言系统音色仅支持 8 种，其余系统音色可能只支持普通话。程序不做音色能力探测，不支持的组合可能仍按普通话合成（D29）。
- 播放/采集的生命周期由 drop 控制：`AudioPlaybackEngine` / `AudioCapture` 一 drop 就停止对应音频流，没有单独的「停止/中断」接口。
- 打字输入句是**排队**语义：当前排队/播放的音频全部播完后才轮到它；界面在提交时就把该句加入字幕历史，可能与实际发声时间有延迟。
- 若提交打字输入时 ASR 当前句尚未句终，该句剩余部分会被丢弃、不会补读（字幕只保留已送 TTS 的前缀，D24）；极端情况下用户持续说话期间打字输入会暂停该句后续合成。
- 打字输入句上限 500 字符；TTS 连续启动失败 3 次时该句按现有机制丢弃（只写日志，不弹错误条）。
- 翻译模式（M3）提供独立「翻译」页：开关、目标语言、粒度、显示原文、翻译模型都可图形化修改并热更新。字幕双语（原文行 + 译文行，两行各自复制），`show_source = false` 时只显示译文（D25/D26）。
- 翻译是逐块 HTTP 请求：clause 模式的实际延迟、端到端 1s 目标、16 语种 TTS 自然度与碎句译文质量尚未做真实语料评估。
- 已送 TTS 的译文不可撤回；翻译失败的片段不发声，只按 D8 显示错误条。
- VRChat 聊天框文本**固定最多 1 条/秒**（本机实测 900ms 长跑跳字），因此聊天框字幕是低频镜像，不是逐事件刷新；GUI 字幕不受影响（D33）。
- 聊天框内容在定稿超过 `retention_secs`（默认 30 秒，1–60）后由客户端主动发空串清空；停止/失败/关闭 OSC/关窗立即清空（D42）。VRChat 自身的聊天框有效期是 VRChat 设置里的选项（2–60 秒，默认 30 秒）：客户端不依赖它，保留时长设得比它长时文本会先被 VRChat 清掉（D45）；当前句（未定稿）不受保留时长限制。
- `/chatbox/typing` 实测不占文本 1 条/秒预算；若未来 VRChat 版本变化，需要重新评估。
- 144 按 UTF-16 码元预算、9 个显示行（含自动换行）；默认 UI 行宽已实测（ASCII 24 / 中文 15 / emoji 12），但 UI 缩放、字体和复杂 ZWJ emoji 仍可能影响换行，截断模型保留保守余量。
- 只支持与本机 VRChat 通信（loopback），不支持远程主机或 OSCQuery 自动发现。
- 没有 UI 自动化测试（D14），UI 与真实音频链路的回归依赖 egui_mcp 运行期观测 + 人工验证。

---

## 15. 翻译模式（M1–M3）

链路：现有 ASR/打字输入 → 本地语言判定与分块 → **翻译 worker**（`client/qwen_mt.rs` + `pipeline/translator.rs`）→ 译文 → 现有 TTS/播放与字幕。

- **开关与字段**：翻译相关字段见 `src/config.rs`（D25）；`enabled = false` 时整条链路与旧行为完全一致。
- **翻译接口**：`POST https://{workspace_host}/compatible-mode/v1/chat/completions`，`model` 取配置中的翻译模型；`messages` 单条 user 文本；`translation_options = { source_lang, target_lang }` **必填**（缺失时模型会退化成普通对话）。源/目标语言直接用 ISO 代码，`source_lang = "auto"` 可兜底；目标语言限 16 种 TTS 语言。
- **worker**：独立 tokio 任务，单 in-flight 顺序处理（`TranslationJob`/`TranslationDone` 带 `generation`，结果按发送顺序回来），主循环不 await HTTP；打字输入会 bump `generation` 并丢弃旧句在途译文。
- **句终屏障**：源文句终时若还有译文在途，等 `pending` 归零后再发「空文本 + 句终」收口 TTS task；打字输入立即收口当前 task 并丢弃未返回译文。译文全部失败时也照样补发句终事件，保证当前字幕行被收口（D26）。
- **屏障期间的 ASR 缓存**：句终屏障未完成时，新到达的 ASR 事件先进 `asr_backlog`，等旧句句终 `Translation` 发出、状态复位后再按序补处理——否则下一句源文会先于旧句句终译文落到界面，出现「一句变两句、原文译文错位」（D28）。
- **分块策略**：`clause`（默认）= 标点/句终切 + 安全尾；`sentence` = 只句终切（见第 9 节）。
- **字幕/事件**：翻译模式下 `PipelineEvent::Asr` 只描述原文进度（`sent_bytes` = 已送翻译源文长度、`is_sentence_end` 恒 false、`language` = 源语言）；`PipelineEvent::Translation{text, sent_bytes, is_sentence_end, language, sentence_id}` 负责译文与句终（`language` 固定为目标语言）。`Timeline` 用 `apply_source` / `apply_translation` 维护双语当前行与历史，同一 `sentence_id` 去重（D26）。
- **双语渲染**：实时页一条消息 = 一个气泡，气泡内自上而下是原文小字弱色行（13pt）、主行大字（20pt）；语言代码画在气泡外侧、朝对话中线的一边（他人气泡右侧 / 我方气泡左侧），不占气泡内的行高；我方气泡靠右、他人气泡靠左（D39）；`show_source = false` 只渲染主行（D26）。
- **失败处理**：翻译失败发 `PipelineEvent::Error`（D8），该片段不送 TTS，后续片段继续。
- **验证**：`cargo test` 覆盖 qwen_mt 协议/重试/错误、chunker 安全尾、config 校验与语言表、字幕双语 reducer；运行期用真实接口 + 打字输入验证 zh→en / zh→ja 双语字幕与源/译文各自复制，`show_source` 热加载即时生效，ASR 语音路径用环境语音间接验证；M3 用「翻译」页验证开关/目标语言/粒度改动触发 D20 热重启、`show_source` 不重启、配置写回保留 `model`。

---

## 16. ASR 预编译热词（D31）

`[asr] hotwords`（字段/限制见 `src/config.rs`）由会话启动时的 `asr::resolve_vocabulary` 同步为百炼预编译热词列表：

1. 空列表 → 直接返回 `None`，不访问网络；
2. 加载 `cache.json`，按 ASR 模型查 `asr_hotwords`；
3. 命中缓存 → `query_vocabulary` 校验（资源不存在则重建；网络异常沿用缓存 ID）；
4. 词表与权重都没变 → 复用；任一项变化且距 `updated_at` ≥5 分钟 → `update_vocabulary` 全量替换并更新缓存（词条全部按权重 50 发送）；
5. 无缓存 → `create_vocabulary`（`target_model` = `[asr] model`，prefix 固定 `bivox`），
   **立即把 `vocabulary_id` 写入 `cache.json`**（同时记录词表与权重快照），再轮询 `query_vocabulary` 到 `status == "OK"`（最多 60s）；
6. 结果交给 `pipeline::session`，注入 ASR run-task 的 `payload.parameters.vocabulary_id`。

- 词条格式：`{"text": "...", "weight": 50}`（超级热词档位，每列表最多 50 条）；不传 `lang` —— 识别同时带 `language_hints` 时只有语种匹配的词条生效，省略后词条对所有语言生效。
- 失败降级：任何一步失败都不阻断会话，发 `PipelineEvent::Warning`，本次识别不带最新热词；有旧 ID 时沿用旧词表。
- 限速：官方建议两次更新间隔 ≥5 分钟；未满时保留旧快照并提示，下次启动自动重试，避免「缓存已更新、百炼未生效、之后不再重试」。
- 管理 API 走 `https://{workspace_host}/api/v1/services/audio/asr/customization`，`model = speech-biasing`，由 `asr/api.rs` 实现；admin 调试 CLI 只做列表/删除（D19）。

---

## 17. VRChat OSC 聊天框（D33）

### 17.1 配置与界面

- `[vrchat.osc]` 字段见 `src/config.rs`；`display` 的三种取值为 `source` / `bilingual` / `translation`；`show_history` 见 D42，`retention_secs` 见 D42/D45。
- `[vrchat.osc]` 不进配置指纹：GUI 写回后 `ChatboxService` 通过 watch 立即读取，不重启实时管线（D20 例外）；`[vrchat] process` 是对方线路的运行配置，进指纹（D37）。
- 「VRChat」页在「语音合成」和「翻译」之间：启用开关、发送端口、三种显示模式、显示历史句、保留时长（1–60 秒，默认 30，悬停提示与 VRChat 自身有效期的关系）、发送测试消息。
  - 翻译模式未开启时三种模式仍可编辑，页面不写额外提示；
  - 无状态行、typing 说明、隐私提示；
  - 测试消息立即发送，不受 1 秒文本节流约束；不改变 typing。
- 顶栏「清除 VRChat 聊天框」只看 `[vrchat.osc] enabled`：启用时可点（与会话是否运行无关），禁用时置灰并在 tooltip 指向启用开关（D44）。

### 17.2 文本合成与截断

入口：`chatbox::compose(&SpeakerView, ChatboxOptions, now) -> Text/Clear`（`SpeakerView` 来自 `timeline.speaker_view(Speaker::Local)`）。

- 候选顺序（D42）：当前句 → 历史句（新→旧，`finished_at` 距今 ≤ `retention_secs`）。关闭历史句时最多装 1 块：有当前句只显示当前句；当前句在所选显示模式下不可渲染时才回退到最后一条历史句。
- 普通模式显示 ASR 文本；翻译模式按 `display` 取源文 / 译文 / 双语。只显示译文且当前句译文未到时，跳过当前句、用保留时长内的历史句填充；仍没有可显示内容则发空串清空（不再保留可能超龄的上一帧）。
- 逐句装填：从 144 码元 / 9 显示行出发；整块放得下就原样放入，放不下就按剩余配额截断该句（保尾 + 前导 `…`）后停止，因此至多一条句子被截断；块间分隔换行各占 1 个码元。
- 当前句不预留历史空间：当前句自己用满预算时历史句不显示。
- 双语：剩余行数为奇数时译文多一行（`ceil(L/2)`），字符按同一比例给基础配额；保留软借用（一侧用不完时借给另一侧）。同一句的「译文\n原文」始终相邻，块间不会交叉。
- 截断细节 `chatbox::truncate_chatbox` / 内部 `fit_*`：
  - 144 UTF-16 码元、最多 9 个**显示行**（包含自动换行；VRChat 超行时保留前 9 行、裁尾）；
  - 实测默认 UI 行宽：约 24 ASCII / 15 中文 / 12 emoji；`chatbox` 用保守单位模型
    （ASCII 3 / CJK 5 / emoji 6，行宽 72）估算自动换行；
  - 超长时从头部删行/删字符，保留句尾；被截掉的句头用前导 `…` 代替（替换首字符，不额外占预算）；
  - 双语的行份额为 `ceil/floor`（剩余 9 行时译文 5 / 原文 4），字符按同一比例分；只有两侧都超过份额时才严格按份额，任一侧用不完就把额度借给另一侧，总计不超过 144 码元 / 9 个显示行，两边都保尾。

### 17.3 发送与限流

- `ChatboxService` 是 `BivoxApp` 持有的常驻 tokio 任务；会话 `emit` 把 `PipelineEvent` 克隆一份发到其无界通道。
- 文本：距上次成功文本发送 ≥ `CHATBOX_TEXT_INTERVAL`（1000ms）才发；1 秒内多次变化只发最新内容；相同文本只在同一段显示内去重，新当前句/打字输入会重置去重。
- typing：`/chatbox/typing` 只在当前句开始/结束时发送；实测不占文本预算。
- 清空（D42）：没有可显示内容时发空串；已定稿内容超过 `retention_secs` 后由客户端主动发空串清空（默认 30 秒；上限 60 秒见 D45，VRChat 自身有效期设得更短时文本会先被 VRChat 清掉，不需要心跳重发）。停止/失败/关闭 OSC/关窗仍立即清空。
- 手动清除（D44）：`ChatboxService::force_clear()` 发 `Command::ForceClear`，`Runtime::force_clear()` 先按上面的 `clear()` 立即发空串并关闭 typing（不等 1 秒窗口、不重试），再 `Timeline::clear_messages()` 丢弃镜像里的当前句与历史句；只清消息，保留翻译模式与最终句去重记忆，避免服务端重复最终结果把句子写回。OSC 关闭时按钮禁用，任务也不发包。
- 发送：`rosc` 编码 `/chatbox/input` `(text, true, false)` 或 `/chatbox/typing`，`std::net::UdpSocket` 发到 `127.0.0.1:port`；不 bind 9000、不用 bundle。
- 失败只写 debug 日志；VRChat 未运行是正常状态，不弹错误。

### 17.4 与 GUI 的边界

- `Timeline` 由 `timeline.rs` 提供，GUI 与 OSC 任务共用，避免 D16/D26 语义漂移；`Message.finished_at` 记录句终时刻，供 OSC 按保留时长裁剪（D42）。
- 手动清除只作用于 OSC 任务自己那份 `Timeline`（`clear_messages`），GUI 字幕历史不受影响（D44）。
- 会话启动失败时，包装任务额外向 OSC 通道发 `StateChanged(Failed)`，让聊天框清空。
- 关窗流程调用 `ChatboxService::shutdown()` 尽力清空并退出任务。

### 17.5 验证

- 单测：配置解析/校验、VRChat 配置写回、文本合成/保尾截断/9 行、typing 跳变、OSC 编码（`/chatbox/input` 的 `sTF`、`/chatbox/typing` 的 bool）、1 秒节流与清空（注入时钟）。
- 历史句（D42）：当前句+历史顺序、保留时长边界、关闭历史句时的最后一句清空、逐句截断与仅截断一条、双语奇数行 3/2 与软借用、译文未到的当前句回退历史、预算用尽不发只有 `…` 的块。
- 手动清除（D44）：`Timeline::clear_messages` 清空消息但保留翻译模式与去重记忆；OSC 侧历史句显示后清除不会复现、重复最终结果仍被去重、OSC 关闭时不发包；`ChatboxService::force_clear` 的命令链路收到空串。
- 2026-11-09 真机复核：144 ASCII 显示 6 行；160 ASCII 在 144 字符处裁尾；12 个显式行只显示 L01–L09；默认 UI 每行约 24 ASCII / 15 中文 / 12 emoji；144 纯中文约 10 行，双语 144 字符会因自动换行超过 9 行而裁掉源文尾。修复后 `chatbox` 按显示宽度估算换行，客户端发的 144 中文/双语样例均为 9 行且保留尾部标记。
- GUI：egui_mcp 验证页签、开关/模式/端口/历史句/保留时长写回、测试消息发包、顶栏清除按钮的可用/置灰与发包。
- 真机：用临时工具实测 1 秒文本间隔、144 字符、typing 每秒切换、保留时长清空与空串立即清空；正式实现后仍需 10 分钟真实语音浸泡。

---

## 18. 他人线路（进程环回 ASR，D36/D40）

`pipeline/others_line` 是会话里的独立 tokio 任务：按 `[vrchat] process` 找到本机进程，用 WASAPI 进程环回采它的音频（`audio/process_capture.rs`），走与麦克风相同的 `asr_line` 接线（VAD → ASR），文本只作为 `Speaker::Others` 事件进时间线。

### 18.1 状态机与重连

```text
Connecting ──成功──▶ Capturing
    ▲                   │ 进程退出 / 采集断流 / ASR 退出
    └───── Waiting ◀────┘
```

- `Connecting`：ToolHelp 找 PID → `ProcessCapture::start(pid)` → 等待环回激活（≤10s）。
- `Capturing`：`asr_line` 在跑，识别事件转发到时间线；本地 VAD 状态发 `VadActive{Others}`。
- `Waiting`：退避 1s → 2s → 4s → 8s → 10s（封顶，无限重试）；每次成功进入 `Capturing` 复位退避。
- **状态跳变才发事件/日志**：进入 `Waiting` 发 `OthersStatus{retry_in}` + 一条 warn；恢复发状态 + 一条 info；等待期间重试失败不重复记。
- 用户主动停止导致的采集结束不算失败（不记告警、不发 `Waiting`）。
- `Waiting` 时该线路的 ASR 任务被停止（run-task `finish-task`），重连后重建，避免空转计费。
- 该线路的 ASR 致命错误走私有错误通道，只触发重连，不升级为会话级 `Error`。

### 18.2 翻译（`[others] translate`）

- 复用 `self_line` 的句子状态、分块器与 `TranslationRuntime`（`pub(crate)`），不另写一套：`SentenceLanguageState` 负责每句语言冻结，`TextChunker` 按 `[translate] granularity` 切块，`TranslationRuntime` 累积译文并按 generation 丢弃过期结果。
- 与自己的区别：**没有 TTS**，译文只发 `PipelineEvent::Translation{speaker: Others}` 进时间线；句终没有「收口 TTS task」这一步，屏障只用于保证下一句源文不与上一句译文配对。
- 翻译 worker 在整个他人线路运行期只建一次，重连不重建；`QwenMtClient` 初始化失败时降级为只显示原文并发一条 `Warning`。
- 源语言与目标语言相同（`[others] target` 与判定语言一致）时跳过 MT，直接把原文作为译文显示。

### 18.3 界面

- 顶部黄色**实时状态**横幅：`Waiting` 显示「原因；N 秒后重试」（每 500ms 重绘刷新倒计时），`Connecting` 显示「正在连接…」，`Capturing` 自动消失；不可手动关闭，会话结束时清空。
- 顶栏他人电平条：`[others] enabled = true` 时占位并按 VAD 状态显示彩色/灰阶；关闭时隐藏但保留空间（D43）。
- 文本只上屏：`Speaker::Others` 事件进 GUI 时间线；OSC 与 TTS 都只消费 `Speaker::Local`。开启 `[others] translate` 后，时间线上一条他人消息是「原文小字行 + 译文大字行」，与自己的双语消息一致。

### 18.4 验证

- 单测：倒计时换算（`retry_seconds`）、时间线的双来源交错、去重与「当前句钉顶」排序。
- 顶栏快捷开关（2026-11-22 真机）：会话运行中切麦克风 → 麦克风线路停止/重建、ASR 重新就绪、会话状态保持 `Running`；切他人 → 黄色状态横幅出现/消失、会话不重启；`config.toml` 与「自己 / 他人」页 checkbox 同步；900×600 下布局不重叠。
- 真机（2026-11-15）：`[vrchat] process` 设为不存在的进程 → 会话照常运行、横幅显示倒计时、日志只记一条 warn；改成 `explorer.exe` 并「应用」→ 触发 D20 重启后他人线路进入 `Capturing`、横幅消失、顶栏出现两个 ASR 就绪日志；停止会话后横幅与指示器清空。
- 双向对话验收（2026-11-15 真机，VRChat 内有真人语音）：
  1. 只麦克风：VAD 指示随说话变化、我方字幕与 TTS/聊天框路径与改造前一致；
  2. 只对方（`mic=false` + `tts=false`）：没有输入/输出设备、没有 TTS 连接，只有他人气泡；
  3. 双向同时：左右气泡交替出现、当前句钉在开头、定稿后才沉入历史；
  4. OSC-only（`tts=false` + OSC 开）：日志「未启用语音合成：跳过音色解析与 TTS」，无 TTS WebSocket，OSC 发送套接字存活（`Get-NetUDPEndpoint` 可见 Bivox 的 127.0.0.1 UDP 端点）；
  5. 只打字上屏（`mic=false` + `tts=false` + 翻译开）：打字出「原文小字 + 译文大字」气泡；
  6. VRChat 未启动 / 运行中退出：横幅倒计时、重开后自动恢复，期间无 ASR 空转；
  7. 运行中保存「自己」页（关合成）：按 D20 热重启，新会话跳过音色解析与 TTS；「他人」页语言列表保存走同一配置指纹机制；
  8. 约 7 分钟连续浸泡（麦克风 + 他人线路 + 翻译同时跑）：除每次启动时的 cpal `Xrun` 告警外无其他 WARN/ERROR，两条线路稳定产出。
