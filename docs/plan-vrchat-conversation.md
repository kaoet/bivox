# 计划：双向语音对话（VRChat 对方语音接入）

> **工作文档**：记录一次改动的冻结需求、重构方案与实施进度。
> 实施完成后，「怎么做」折叠进 [`architecture.md`](./architecture.md)，长期取舍进 [`decisions.md`](./decisions.md)，
> 用户可见行为进 [`README.md`](../README.md)；本文件随后删除（git 历史保留）。
> 配置字段的最终事实源仍然是 [`src/config.rs`](../src/config.rs)，本文件中的 TOML 只是方案快照。

**状态**：需求已冻结；P0–P5 已完成，P6（对方翻译 + 打字输入）待开工。

---

## 1. 目标

在现有「麦克风 → ASR →（翻译）→ TTS/OSC」之外，新增一条**方向相反**的线路：侦听 VRChat 自己的音频输出，做 ASR，可选翻译成本地语言，**只上屏**（不进 OSC、不做 TTS）。两条线路的结果在同一条聊天气泡时间线上左右分栏显示。

典型用途：与日语玩家对话——自己说中文（语音或打字）→ 译成日语 → 进 VRChat 聊天框并合成语音；对方说日语 → 译成中文 → 显示在本机界面。

---

## 2. 冻结的需求

### 2.1 配置 schema

```toml
[api]
key = "sk-..."
workspace_host = "..."

# ── 两条线共用 ───────────────────────────────
[asr]
model = "qwen-audio-3.1-asr-flash-streaming"
hotwords = ["VRChat", "贵咩", "欧润橘"]      # 共用；同一个 vocabulary_id

[translate]
model = "qwen-mt-flash"
granularity = "clause"                      # clause / sentence
show_source = true                          # 气泡是否显示原文小字行

[tts]
model = "qwen-audio-3.1-tts-flash"
voice = "custom:misha"
dialect = "普通话"
[tts.custom_voices.*] ...

# ── 我方线路 ─────────────────────────────────
[self]
mic = true                  # 是否采集麦克风（false = 只能用打字输入）
languages = ["zh"]          # 有序，首个为默认；API 只取前 4 个当 language_hints
translate = true
target = "ja"               # 我方译文语言
tts = true                  # 是否合成语音并播放

# ── 对方线路 ─────────────────────────────────
[others]
enabled = true
languages = ["ja"]
translate = true
target = "zh"               # 对方译文语言

# ── VRChat ──────────────────────────────────
[vrchat]
process = "VRChat.exe"      # 进程树环回采集目标
[vrchat.osc]
enabled = true
port = 9000
display = "translation"

# ── 本地 ────────────────────────────────────
[audio]
input_device = "HyperX 耳机 (HyperX Cloud III)"
output_device = "..."
[vad] ...
[chunking] ...
[cache] ...
```

旧字段直接替换，不做兼容（AGENTS.md「配置变更」）：`[asr].languages` → `[self].languages`；`[translate].enabled/target` → `[self].translate/target`。

### 2.2 能力启用（没有「线路总开关」）

| 组件 | 启用条件 |
|---|---|
| 麦克风采集 + 我方 ASR | `[self].mic` |
| 我方 TTS（含音色解析、播放引擎） | `[self].tts` |
| OSC 聊天框 | `[vrchat.osc].enabled` |
| 我方翻译 worker（服务麦克风与打字输入） | `[self].translate` |
| 对方线路（进程环回 + 对方 ASR） | `[others].enabled` |
| 对方翻译 worker | `[others].enabled ∧ [others].translate` |
| **打字输入** | 会话 `Running` 时**始终可用** |
| 气泡时间线 | 始终存在 |

- TTS 与 OSC 都关、翻译关闭 → 打字输入只上屏原文；
- TTS 与 OSC 都关、翻译开启 → 打字输入上屏「译文 + 原文」，不发声、不进聊天框；
- 最极端配置（只有打字 + 上屏）也照常启动，没有「无线路则拒绝开始」的分支；
- 启动前仍检查 API Key（沿用现状）。

### 2.3 启停与失败

- **启动**：所有已启用组件初始化必须成功（设备、ASR 连接、音色解析、热词同步）。
- **唯一例外**：VRChat 进程不可达不算启动失败（见 2.4）。
- **运行期**：ASR/TTS/翻译服务错误 → 整会话 `Failed` + 红色错误条（沿用现有语义）。
- `[self]` / `[others]` / `[vrchat].process` 全部进配置指纹，运行中改动触发热重启（D20）。

### 2.4 对方线路：进程不可达 = 临时错误 + 自动重连

| 状态 | 行为 |
|---|---|
| `Connecting` | 立即按进程名找 PID 并尝试开采集 |
| `Waiting` | 找不到进程 / 进程退出 / 采集断流 → 黄色横幅 + 倒计时，后台重试 |
| `Capturing` | 采集正常，对方 ASR 与翻译在跑 |

- 重连节奏：1s → 2s → 4s → 8s → 10s 封顶，**无限重试**，成功即复位退避。
- **横幅内容带倒计时**，例如：`对方语音：未找到 VRChat.exe，8 秒后重试`；倒计时每秒刷新；正在尝试时显示 `对方语音：正在连接 VRChat…`。
- 该横幅是**实时状态**而不是一次性告警：不提供手动关闭，恢复后自动消失。
- 日志**按状态跳变记录**：进入 `Waiting` 记 1 条 warn；`Waiting` 期间每次重试失败不记日志；→ `Capturing` 记 1 条 info（含 PID）；之后再次掉线再记 1 条 warn。
- 采集断开时**同步拆除对方 ASR 连接**（`finish-task`），重连成功后再重建，避免空转计费。
- 时间线顶部同时显示一行弱色状态文案（`等待 VRChat 进程… / 已连接（PID 1234）`），不依赖日志与横幅。

### 2.5 界面

- 单一**气泡时间线**，**最新在上**（与现有阅读习惯一致），滚动锚在顶部。
- 我方气泡右对齐、对方气泡左对齐；单条最大宽度约 70%。
- 气泡两行：主行 = 译文（`translate=false` 时是原文，大字）+ 副行 = 原文（小字弱色，受 `show_source`）；两行各自悬停复制。
- **主行按「已送入下游」的字节进度分色**：我方有 TTS 时按 TTS 进度；我方关 TTS 或对方气泡时按翻译进度。
- 语言代码为气泡角上的小字角标，取消现有左侧 32pt 语言列。
- 当前未完成的句子是最上面的气泡之一，随 ASR/翻译修订实时更新；句终后定型。
- 历史合并上限 500 条；停止/失败只清当前未完成气泡，历史保留。
- 顶栏 VAD 指示器变成两个：`我方 ●` / `对方 ●`；未启用的不显示。
- 打字输入框仍在底部，任何时候 `Running` 都可提交。

### 2.6 明确不做

- 说话人分离（百炼实时 ASR 不支持，见第 7 节）；对方气泡不标说话人。
- 对方文本进 OSC 聊天框、对方文本 TTS。
- 多路监听（同时听 VRChat + Discord）。
- 对话历史落盘/导出。
- 系统环回（A2 路线）；只做进程环回（A3）。

### 2.7 设置页签：拆出「自己」「他人」两个页

现有页签把「线路设置」和「共用设置」混在一起（语言列表在「语音识别」、翻译开关/目标语言在「翻译」），有第二条线路之后会出现「这条线用的是哪个语言列表」的歧义。改成**每条线路一个页签**：

| 页签 | 字段 |
|---|---|
| **自己** | `[self] mic`、`languages`、`translate`、`target`、`tts` |
| **他人** | `[others] enabled`、`languages`、`translate`、`target` |

留在原页的只有**两条线路共用**的部分：

| 页签 | 字段 |
|---|---|
| 语音识别 | `[asr] model`、`hotwords` |
| 翻译 | `[translate] model`、`granularity`、`show_source` |
| 语音合成 | `[tts] model`、`voice`、`dialect`、`custom_voices`（**不并入「自己」页**） |
| 音频 | `[audio]` 设备、`[vad]` |
| VRChat | `[vrchat] process`（侦听哪个进程）、`[vrchat.osc]` |
| 设置 | `[api]`、`[cache]`、`[chunking]` |

- 页签顺序：**实时 / 音频 / 自己 / 他人 / 语音识别 / 语音合成 / VRChat / 翻译 / 设置**。
- 两个新页沿用 Tab 级「保存 / 放弃更改」（D22/D32）；所有字段都进配置指纹，运行中保存按 D20 热重启。
- 「自己」页的语言列表直接复用现有「语音识别」页的有序列表控件（拖拽排序 / 删除 / 追加）。
- `[vrchat] process` 归「VRChat」页：这是该页当初预留的 VRChat 专属设置位，不塞进语言/翻译页。
  - 它是文本框 + 「应用」按钮（仿翻译模型的交互），**不能每敲一个字就写回**——否则输入过程中会反复触发热重启。
  - 该页其余 OSC 字段仍是「改值即保存、不重启」；两种语义在同一页共存，页面内要写清楚。
- 「自己」「他人」两页是 `[self] mic` / `[self] tts` / `[others] *` 唯一的 GUI 入口；在此之前它们只能手改 `config.toml`。

---

## 3. 重构方案

### 3.1 模块

```text
src/audio/process_loopback.rs   NEW  WASAPI 进程环回（ActivateAudioInterfaceAsync + PROCESS_LOOPBACK）
src/audio/process_capture.rs    NEW  采集线程：PID 发现 → 环回 → 100ms 块 → channel（含静音补齐）
src/pipeline/asr_line.rs        NEW  共用：chunk 流 → VAD 线程 → ASR 任务 → AsrEvent 流
src/pipeline/self_line.rs       NEW  我方：采集/ASR(可选) + 打字输入 + 语言判定 + 分块 + 翻译 + TTS/OSC
src/pipeline/others_line.rs     NEW  对方：采集 + 重连状态机 + VAD + ASR + 语言判定 + 翻译 → 时间线事件
src/pipeline/session.rs             瘦身成会话编排：启动顺序、事件发射、取消、收尾
src/timeline.rs                 NEW  时间线状态机（替代 subtitles.rs）
src/subtitles.rs                DEL
src/events.rs                       加来源标记，新增 OthersStatus
src/config.rs                       schema / 默认值 / validate / 指纹
src/gui/realtime_tab.rs             重写为气泡时间线
src/gui/vrchat_tab.rs              加 `[vrchat] process` 输入框
src/gui/typed_input.rs              提交语义调整（不再假设我方线路存在）
src/gui/toolbar.rs                  双 VAD 指示器 + 倒计时横幅（或独立状态组件）
src/gui/self_tab.rs             NEW  「自己」页：mic / 语言列表 / 翻译 / tts
src/gui/others_tab.rs           NEW  「他人」页：enabled / 语言列表 / 翻译
src/gui/asr_tab.rs                  缩到 model / hotwords（语言列表搬走）
src/gui/translate_tab.rs            缩到 model / granularity / show_source（开关与目标语言搬走）
src/app.rs                          事件处理、状态字段、指纹
src/chatbox.rs / src/osc.rs         只消费 `Speaker::Local` 事件
```

**先搬家后加功能**：把我方现有调度循环（D24/D26/D28 的 backlog/屏障/丢弃逻辑）**原样**搬进 `self_line.rs`，只换事件签名与数据源，跑通全部门禁与冒烟后再叠加新能力。

### 3.2 事件契约

```rust
pub enum Speaker { Local, Others }

Asr { speaker, text, is_sentence_end, sent_bytes, language, sentence_id }
Translation { speaker, text, sent_bytes, is_sentence_end, language, sentence_id }
TypedSentence { text, language }          // 始终 Local；语义 =「已受理的一句我方文本」
VadActive { speaker, active }
OthersStatus { state: OthersLineState, detail: String, retry_in: Option<Duration>, pid: Option<u32> }
// StateChanged / Devices / VoiceInfo / Warning / Error 不变

pub enum OthersLineState { Connecting, Capturing, Waiting }
```

- OSC 与 TTS 只消费 `Local`；对方事件只进时间线。
- `retry_in` 只描述「下一次何时重试」，GUI 收到后存 `Instant::now() + retry_in` 自行倒计时，并在激活期间 `request_repaint_after(500ms)`。
- 新增/修改事件变体时同步 `events.rs`、`app.rs`、decisions、architecture（D6 约束）。

### 3.3 会话与重连状态机

```text
会话：Idle → Starting → Running → Stopping → Idle
                 └→ Failed

对方线路（在 Running 内独立推进）：
  Connecting ──成功──▶ Capturing
      ▲                    │
      │                进程退出 / 采集断流
      └──── Waiting ◀──────┘
```

- 采集线程是 `std::thread`（不占 tokio runtime），100ms 攒块进有界 channel。
- 每次进入 `Capturing` 才 spawn 对方 VAD 线程与 ASR 任务；离开时 `finish-task` 并 join/丢弃。
- 采集层无音频包时按墙钟补静音包，保证 ASR run-task 的 keepalive 不断（见第 6 节）。
- 停止会话：置取消标志 → 采集线程退出 → 关闭 channel → 等 ASR 收起（≤3s）→ 发 `Idle`。

### 3.4 时间线状态机（`src/timeline.rs`）

```rust
pub struct Message {
    pub speaker: Speaker,
    pub open: bool,              // 未完成 = 正在识别/翻译
    pub language: String,        // 主行语言
    pub text: String,            // 主行（译文案；不翻译时 = 原文）
    pub sent_bytes: usize,       // 主行已送下游的字节前缀
    pub source: String,          // 副行原文
    pub source_language: String,
    pub sentence_id: Option<u64>,
}

pub struct Timeline {
    pub translation: [bool; 2],              // 按方向的翻译开关
    pub messages: Vec<Message>,              // 索引 0 = 最新
    pub last_finalized: [Option<u64>; 2],    // 按方向的 sentence_id 去重记忆
}
```

- 不变量：每个方向至多一条 `open` 消息；新消息插入 `messages[0]`；同一 `open` 消息原地更新（不改位置）。
- 去重按 `(方向, sentence_id)`——两条线来自不同 ASR 服务，id 空间互不相干。
- 上限 500 条，超出丢弃最旧（队尾）。
- 接口：`apply_source` / `apply_translation` / `push_typed` / `reset_open` / `speaker_view(Speaker)`。
- `chatbox` 用 `speaker_view(Speaker::Local)` 得到「我方当前句 + 最后一条我方历史」，保持现有聊天框语义。

### 3.5 配置校验与指纹

`validate` 新增/调整：

- `[self].languages`、`[others].languages`：非空、每项 ∈ `ASR_LANGUAGES`、去重（沿用现有规则）；
- `[self].target`：仅 `[self].translate` 时校验 ∈ `TRANSLATION_LANGUAGES`；
- `[others].target`：仅 `[others].enabled ∧ [others].translate` 时校验；
- `[vrchat].process`：非空字符串（省略走默认 `VRChat.exe`，显式空串非法，D35）；
- 删除 `[asr].languages`、`[translate].enabled`、`[translate].target` 及其校验。

指纹新增：`self.{mic,languages,translate,target,tts}`、`others.{enabled,languages,translate,target}`、`vrchat.process`。
不入指纹：`translate.show_source`（只影响渲染）、`[vrchat.osc].*`（D33）。

### 3.6 GUI

- `realtime_tab.rs` 重写：`ScrollArea` 锚顶（偏移 0 即 stick top），按 `messages` 顺序自上而下渲染；左右对齐、最大宽度、角标、进度分色、悬停复制。
- 倒计时横幅：新状态字段 `others_status: Option<OthersStatusView>`，在横幅区渲染；激活期间每 500ms 请求重绘。
- `toolbar.rs`：`我方 ●` / `对方 ●` 两个 VAD 灯。
- 空状态提示按启用能力变化（没开麦克风时不再提示「说话」）。
- 新增 `self_tab.rs` / `others_tab.rs` 两个页签（见 2.7）；`asr_tab.rs` 去掉语言列表、`translate_tab.rs` 去掉开关与目标语言；`Tab::ALL` 顺序同步更新。

### 3.7 文档

- `docs/decisions.md`：新增 D38（设置页签按线路拆分，细化 D22/D27/D32）、D39（事件来源与气泡时间线，细化 D6/D16）、D40（对方线路重连与状态横幅，细化 D20/D8）。
- `docs/architecture.md`：系统概览与数据流、代码结构、事件模型、GUI、配置、已知限制。
- `README.md`：界面表（含「自己」「他人」两页）、使用姿势、故障排查（VRChat 进程）。
- `AGENTS.md`：文档导航保持只引用。
- `src/config.rs`：字段注释、`Default`、`validate`。

---

## 4. 阶段任务

- [x] **P0 采集层**：`process_loopback.rs`（kamoai 实现移植）+ `process_capture.rs`（PID 发现、攒块、断流、静音补齐）+ `loopback_debug` 调试工具；D36 已落地。
  真机验证（2026-11-15）：对播放 WAV 的独立进程采到 6.1 秒 |RMS| −24.8 dBFS / 峰值 −10.0 dBFS 的真实音频（WAV 独立复核）；目标进程静音时稳定输出 10 块/秒（−96.7 dBFS）；目标进程退出后 ~1 秒内被存活检测发现并结束采集。
- [x] **P1 配置**：新 schema、默认值、`validate`、指纹；删除旧字段；单测覆盖。D37 已落地。
  落地内容：`[asr]` 只剩 `model`/`hotwords`；新增 `[self]`（`mic`/`languages`/`translate`/`target`/`tts`）与 `[others]`（`enabled`/`languages`/`translate`/`target`）；`[translate]` 只剩 `model`/`granularity`/`show_source`；`[vrchat] process` 默认 `VRChat.exe`；`[self]` 在 TOML 里就是 `self`（Rust 字段叫 `local`）。
  GUI 验证（2026-11-15）：`EGUI_INSPECTION=1` 启动成功、无配置错误横幅；翻译页显示 `[self] target = ja`，语音识别页显示 `[self] languages = zh/en/ja`；在翻译页勾选/取消「启用翻译模式」各写回一次，生成的 `config.toml` 含 `[self] languages/target/translate` 且 `[asr]` 不再有 `languages`。本地 `config.toml` 已按新 schema 改写（保留 API Key 与音色表）。
- [x] **P2 设置页签重组**：新增「自己」「他人」两页，搬移 `[self]` / `[others]` 字段；`[vrchat] process` 加到「VRChat」页；「语音识别」「翻译」页缩到共享项；页签顺序与 D38 同步。D38 已落地。
  落地内容：`gui/self_tab.rs`、`gui/others_tab.rs`、`gui/language_list.rs`（两页复用的有序语言列表）；`Tab::Local` / `Tab::Others`，顺序 **实时 / 音频 / 自己 / 他人 / 语音识别 / 语音合成 / VRChat / 翻译 / 设置**；`[vrchat] process` 用文本框 + 「应用」；语音识别页只剩模型/热词，翻译页只剩粒度/显示原文/模型。
  GUI 验证（2026-11-15）：九个页签顺序与截图一致；「自己」页关「合成语音并播放」→ 保存后 `[self] tts = false` 落盘、恢复后该键消失；「他人」页开/关「侦听他人语音」→ 保存后 `[others] enabled` 正确写入；VRChat 页改监听进程并「应用」→ `[vrchat] process` 落盘；语音识别页不再有语言列表、翻译页不再有开关与目标语言。
- [x] **P3 契约与纯逻辑**：`Speaker` + 事件来源；`timeline.rs`；`chatbox` 投影；单测。D39 已落地。
  落地内容：`events::Speaker { Local, Others }`，`Asr` / `Translation` / `VadActive` 带 `speaker`；`timeline::Timeline` 替代 `subtitles.rs`（消息带来源、每条线路至多一条未完成消息、按 `(speaker, sentence_id)` 去重、合并上限 500）；`chatbox::compose(&SpeakerView, display)`；OSC 只消费 `Local`；实时页改为按时间线渲染（布局暂不变，P7 再改气泡）。
  验证（2026-11-15）：门禁全绿（lib 244）；GUI 启动后实时页空状态正常；真实启动会话后用打字输入提交一句，消息以 `zh` 语言列 + 大字行出现在时间线顶部、输入框清空。
- [x] **P4 会话拆分**：`asr_line` / `self_line` / 会话瘦身；我方行为零变化；全门禁 + GUI 冒烟。
  落地内容：`pipeline/asr_line.rs`（chunk → VAD 线程 → ASR 任务 → `AsrEvent`，我方与他人线路共用）、`pipeline/self_line.rs`（我方调度循环原样搬移：语言判定/分块/翻译/打字输入/TTS 边界，886 行）、`pipeline/session.rs` 从 1174 行降到 377 行，只做建连、设备、音色、翻译 worker 与收尾。
  验证（2026-11-15）：门禁全绿（lib 244）；GUI 冒烟：开启翻译 → 开始会话 → 打字输入「你好，今天天气不错。」→ 时间线显示 `zh` 原文行 + `ja` 译文行（走完 self_line 的翻译 → TTS 路径）；停止后状态回空闲、配置已恢复。
- [x] **P5 对方线路**：采集 + 重连状态机 + `OthersStatus` + 倒计时横幅 + 双 VAD 指示器（不含翻译）。D40 已落地。
  落地内容：`pipeline/others_line.rs`（独立 tokio 任务：找 PID → 进程环回 → 复用 `asr_line` → 只上屏；`Connecting/Capturing/Waiting` 状态机、1→2→4→8→10s 退避、按跳变发状态与日志、等待时拆掉 ASR 连接）；`events::OthersStatus`；顶部带倒计时的实时状态横幅（不可手动关闭、恢复自动消失）；顶栏 `他人 ●` 指示器；他人 ASR 错误走私有通道只触发重连。
  验证（2026-11-15）：门禁全绿（lib 245，新增倒计时换算测试）；GUI：`[others] enabled` 后顶栏出现 `他人 ●`；进程设为不存在的名字 → 会话正常运行、横幅「未找到进程 NoSuchApp.exe；1 秒后重试」且日志只记一条 warn；改为 `explorer.exe` 并应用 → D20 重启后 `已连接进程 explorer.exe（PID …）`、横幅消失、两条 ASR 就绪日志；停止后横幅与指示器清空。
- [ ] **P6 对方翻译 + 打字输入**：对方 MT worker；`tts=false` 时的打字输入输出路径；双向联调。
- [ ] **P7 界面**：气泡时间线重写（最新在上、左右对齐、角标、分色、复制）。
- [ ] **P8 收尾**：文档四件套、门禁、真机冒烟。

每阶段结束跑 `CONTRIBUTING.md` 第 5 节门禁；GUI 改动用 egui_mcp + 截图验证。

---

## 5. 验收场景

1. 只麦克风（现有行为回归）：开始/停止、字幕、TTS、OSC、热重启。
2. 只对方：`[self].mic=false`、`tts=false`、OSC 关 → 只上屏对方译文。
3. 双向同时在跑：双方交替说话，气泡左右交错、顺序正确。
4. OSC-only：`tts=false` + OSC 开 → 不解析音色、不建 TTS，聊天框照常更新。
5. 只打字上屏：`mic=false`、`tts=false`、OSC 关、翻译开 → 打字出「译文 + 原文」气泡。
6. VRChat 未启动时点「开始」：会话进入 `Running`，横幅带倒计时；启动 VRChat 后自动连上、横幅消失、日志一条 info。
7. 运行中关闭 VRChat：横幅 + 倒计时；重开后恢复；期间无 ASR 空转。
8. 改 `[others].languages` 运行中保存 → 热重启、对方线路用新语言列表。
9. 两个新配置页：在「自己」页关掉合成语音并保存 → 运行中热重启、不再建 TTS；在「他人」页打开 `enabled` 并填进程名 → 保存后对方线路按新配置启动。

---

## 6. 实现中要注意的坑

1. **安静期补静音**：进程环回可能长时间不送音频包（kamoai 已注明），而 ASR run-task 约 23s 无音频会失败；采集层必须按墙钟补静音块，不能只等包。
2. **采集线程不能进 tokio**：`std::thread` + 有界 channel；积压 >1s、`DATA_DISCONTINUITY`、10s 无包都按「采集断开」处理，走 `Waiting`。
3. **对方 ASR 随采集启停**：不要长期挂着一条没人喂音频的 WS。
4. **`sentence_id` 跨方向会碰撞**：去重必须带方向。
5. **两个 `LanguageTracker`**：粘性语言不能互相污染。
6. **Silero 实例**：每次进入 `Capturing` 新建（加载约 36ms），或复用并 `reset_transient`；两者都可，按实现简单度选。
7. **`[vrchat].process` 在 `[others].enabled=false` 时不被使用**，但仍要合法。
8. **`tts=false` 跳过音色解析**：`voice::resolve_active_voice` 可能上传复刻，必须按需调用。
9. **两条 WS 连接**：一条连接上不能并行跑两个 run-task，两条线各用一条。

---

## 7. 调研结论：说话人分离（结论：不做）

- 百炼**实时** ASR 全系列都不返回说话人标签：`qwen-audio-3.1-asr-flash-streaming` 的能力表没有该项；Paraformer 实时 `run-task` 无 `diarization_enabled`、`result-generated` 无 `speaker_id`。
- 只有非实时模型支持（`qwen-audio-3.1-asr-flash` / `-filetrans` / `fun-asr`），是「提交整段音频」的同步/异步形态，延迟不可用于对话。
- 即使可用，阿里云自己的实测文章也指出欠分/过分裂并存、人数提示无效、标签附在「行」而非「句」上，多人重叠说话无法分离。
- 若要「谁在说」，唯一现实路径是本地声纹聚类（ONNX speaker embedding + 在线聚类），列为 v2 可选。

参考：<https://help.aliyun.com/zh/model-studio/asr-model>、<https://help.aliyun.com/zh/model-studio/paraformer-client-events>、<https://help.aliyun.com/zh/model-studio/paraformer-server-events>、<https://developer.aliyun.com/article/1779101>

---

## 8. 风险

- **进程环回**依赖 Windows 10 2004+（本机 Win11 26H2 可用）；`windows 0.62.2` 已在 `Cargo.lock`（wgpu 30 引入），不新增版本。
- VRChat 语音经过 Opus 压缩与空间衰减，远处玩家识别率低；游戏 BGM 会造成噪音识别——属物理限制。
- 第二条 ASR 流常驻计费（VAD 门控后仍按时长计费）。
- 我方调度循环搬迁（P4）是行为回归风险最高的一步，必须有回归场景兜底。
