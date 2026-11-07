# 翻译模式实施计划

> **状态：计划（尚未实现）。**
>
> 本文件是阶段性开发计划，不是项目的长期事实来源。实现过程中产生的长期结论按 [CONTRIBUTING.md](../CONTRIBUTING.md) 第 7 节归位：设计取舍进 [decisions.md](./decisions.md)（新增 `D25`、`D26`，并修订 `D3`/`D6`/`D16`），模块与数据流进 [architecture.md](./architecture.md)，配置字段以 [config.example.toml](../config.example.toml) 为准，用户可见行为进 [README.md](../README.md)。功能完成、文档归位后删除本文件。

## 1. 目标

用户打开翻译模式并指定目标语言后：

- 麦克风说话，或打字输入；
- 软件把**目标语言的文本显示为字幕**（默认双语：译文为主、原文为辅）；
- 软件用**当前音色朗读目标语言的译文**。

### 非目标（v1 不做）

- 不做端到端实时同传（`qwen3.8-livetranslate-flash-realtime` 一路）。理由见第 2 节；未来可作为独立的“同传模式”。
- 不做“文本 → 目标语言语音”的独立接口，沿用 MT + 现有 TTS。
- 目标语言不超出 TTS 官方支持的 16 种（见 3.3）。

## 2. 已确认的决定

| 项 | 决定 |
|---|---|
| 链路 | **A 方案**：现有 ASR → `qwen-mt-flash` 文本翻译 → 现有 TTS。不引入端到端同传模型 |
| 字幕 | 默认**双语**：译文大字为主，原文小字弱色，可在配置里关闭原文 |
| 翻译粒度 | **做成配置开关**（按标点/短句即翻 vs 整句结束再翻），由实机测试决定默认以外的取舍 |
| 目标语言 | 先限制为 TTS 官方 16 种（`CLONING_LANGUAGES`） |
| 开关位置 | 新增独立的「翻译」Tab，翻译选项集中在 Tab 内；字段写入 `config.toml`，GUI 可改 |
| 自定义音色缺目标语言样本 | 沿用现有回退行为（用默认语言样本），**不加** README 说明、日志或 UI 提示 |

选 A 而不选端到端同传的原因：同传模型不能输入文本（打字输入必须另开通道）、音色体系不通用（需按翻译模型重新注册，与现有自定义音色不互通）、协议是另一套 `/api-ws/v1/realtime`、限流仅 RPM 10 且成本约 8 元/小时。A 方案只多一次百毫秒级 HTTP 往返，保留打字输入、自定义音色、逐句可控字幕，且无需新增 Rust 依赖。

## 3. 总体设计

### 3.1 数据流

```
麦克风 → ASR（不变）→ 源语言判定（复用 LanguageTracker）
      → 分块（翻译模式只切标点/句终，标点须离开 ASR 不稳定尾部）
      → 翻译 worker（qwen-mt-flash，顺序处理）
      → 现有 TTS（language=目标语言，voice=按目标语言选择的音色）
      → 播放（不变）

字幕：ASR 源文事件 + 翻译结果事件 → SubtitleState（双语、双进度）→ 实时页
打字：GUI → TypedInput → 源语言判定 → 翻译 worker → TTS + 字幕历史行
```

### 3.2 模块改动

| 文件 | 改动 |
|---|---|
| `src/client/qwen_mt.rs`（新增） | 翻译客户端：请求构造、响应解析、超时/重试、错误 |
| `src/client/mod.rs` | 导出新模块 |
| `src/pipeline/session.rs` | 接入翻译阶段与句终屏障、目标语言 TTS、打字翻译 |
| `src/pipeline/chunker.rs` | 新增“标点/句终切 + 安全尾保护”的翻译模式策略（默认行为不变，现有测试保持） |
| `src/pipeline/translator.rs`（新增，可选） | 翻译 worker 与句终状态机；抽成可单测结构 |
| `src/config.rs` | `TranslateConfig`、归一化、校验、URL 推导 |
| `src/config_edit.rs` | `set_translate` 定点写回 |
| `src/language.rs` | `TRANSLATION_LANGUAGES`、语言显示名（ISO 代码直接用于 MT，无需名称映射） |
| `src/events.rs` | 新增 `Translation`、`TranslatedSentence` 事件 |
| `src/app.rs` | 双语 `SubtitleState`、事件分发、配置指纹、启动参数、`Tab` 枚举新增「翻译」页 |
| `src/gui/translate_tab.rs`（新增） | 独立的「翻译」页：开关、目标语言、粒度、显示原文；模型只读 |
| `src/gui/realtime_tab.rs` | 双语行渲染，原文/译文各自的复制按钮 |
| `src/gui/mod.rs` | 注册新 tab 模块 |
| `config.example.toml` / `README.md` / `docs/architecture.md` / `docs/decisions.md` | 见第 13 节 |

无新增依赖：`reqwest` 已有 `json`；短句非流式翻译不需要 `stream` feature。

### 3.3 目标语言集合

在 `src/language.rs` 新增：

```rust
/// 翻译模式的目标语言：qwen-audio-3.1-tts-flash 官方支持的 16 种合成语言。
pub const TRANSLATION_LANGUAGES: &[&str] = CLONING_LANGUAGES;
```

- 归一化复用 `canonical_language`（`tl` → `fil`）。
- 新增 `language_display_name(code)`（中文名，未知时回退代码），供下拉框使用。
- qwen-mt 支持 92 种互译，但目标语言先限制在“念得出来”的 16 种；扩语言等 TTS 覆盖扩展后再做。

## 4. 配置设计

```toml
[translate]
# 翻译模式：把输入（麦克风/打字）翻译成 target 指定的语言后再合成与显示字幕。
enabled = false
# 目标语言代码，见 src/language.rs 的 TRANSLATION_LANGUAGES。
target = "en"
# 翻译模型（qwen-mt-flash 支持增量流式，当前按短句非流式调用）。
model = "qwen-mt-flash"
# 翻译粒度：
#   "clause"   = 有标点/句终就翻译，延迟低，句子可能偏碎；
#   "sentence" = 整句结束再翻译，质量优先，延迟约一整句。
granularity = "clause"
# 字幕是否在译文下方以弱色小字显示原文。
show_source = true
```

- `src/config.rs` 新增 `TranslateConfig` 并挂到 `AppConfig`；`deny_unknown_fields` 会拒绝旧读取，须同步结构体与模板。
- `normalize`：`target = canonical_language(target)`；`granularity` 只接受 `"clause"` / `"sentence"`。
- `validate`：`enabled = true` 时 `target` 必须属于 `TRANSLATION_LANGUAGES`、`model` 非空；非法时整份配置无效（沿用现有错误展示）。
- `config_signature` 必须包含会影响运行会话的 translate 字段（`enabled`/`target`/`model`/`granularity`），漏一个会导致热更新静默失效；`show_source` 只影响渲染，不参与指纹（改它不重启）。
- `config_edit::set_translate(...)`：`ensure_table("translate")` + `set_or_remove`，按 GUI 改动的字段逐项写，走现有 `save`（整文件校验 + `.bak`）。
- D3 白名单扩展：`[translate] enabled`、`target`、`granularity`、`show_source` 由 GUI 写；`model` 只读（翻译页展示，手改配置后「重新加载配置」生效）。

## 5. 翻译客户端（`src/client/qwen_mt.rs`）

### 5.1 协议（2026-11-07 实测确认）

- 端点：`https://{workspace_host}/compatible-mode/v1/chat/completions`，复用 `[api] workspace_host`。**已用当前 workspace 专属域名实测可用**（200）；原生端点 `/api/v1/services/aigc/text-generation/generation` 也可用，但 v1 只走兼容端点，不留双实现。
- 请求（非流式）：

```json
{
  "model": "qwen-mt-flash",
  "messages": [{ "role": "user", "content": "待翻译文本" }],
  "translation_options": {
    "source_lang": "zh",
    "target_lang": "en"
  }
}
```

- `translation_options.source_lang` / `target_lang`：**ISO 代码（`zh`/`en`/`ja`…）与英文语言名（`Chinese`/`English`…）都接受，实测输出一致**。实现直接透传项目内部的 ISO 语言代码，`language.rs` **不需要** ISO→MT 名映射；`source_lang: "auto"` 也支持，作为源语言判定不可靠时的兜底。
- **`translation_options` 必须发送**：实测缺省时模型不翻译，而是当普通对话模型回答（把“今天天气不错……”当闲聊回了一段建议）。客户端把 options 设为必填参数；`config.validate` 已限定 `target ∈ TRANSLATION_LANGUAGES`，请求构造处不再给默认值。
- 16 种目标语言（`CLONING_LANGUAGES`）**全部实测 200**（含 `fil`），`source_lang: "auto"` 也验证通过。
- 非 2xx 返回 `{ "error": { "code": "...", "message": "..." } }`；非法目标语言实测 400 `invalid_parameter_error` / “暂时不支持当前设置的语种！”，客户端直接取 `message` 作为错误详情。
- 响应取 `choices[0].message.content`；空串按失败处理。
- 延迟实测（短句约 20 字）：非流式总耗时 0.19–0.31s；`stream: true` 首包约 0.12s、总耗时 0.21s。v1 用非流式足够；流式留作后续优化（兼容端点确认支持 SSE，逐块 `data: {...}`）。

### 5.2 超时、重试、错误

- 独立 `reqwest::Client`：连接超时 5s、整请求超时 10s（翻译是延迟敏感路径，不能沿用音色 API 的 120s）。
- 重试：网络错误 / 5xx / 429 重试一次（300ms 退避，429 用 1s）；仍失败则把错误返回给会话。
- 失败**不终止会话**：会话 `emit PipelineEvent::Error`（D8 红条），跳过该段，保证句终收口。
- `Err` 文案带阶段（“翻译请求失败”“翻译响应解析失败”），便于日志排查。

## 6. 会话接入（`src/pipeline/session.rs`）

### 6.1 分块策略

`TextChunker` 增加一个构造选项（例如 `with_word_cut(false)` / `stable_punctuation_tail(true)`，或 `ChunkPolicy`），翻译模式下：

- **clause 粒度**：只用标点切分 + 句终切分，关掉按词数/字数的兜底切分；并给标点切分加**安全尾保护**。
  - 核对现状：现有 `guard_tail_chars`（默认 3）只作用于“无标点时按字/词兜底硬切”；标点分支是“见到即切”（`test_punctuation_split` 断言 `"你好，世界！"` 一次切成两段）。[architecture.md](./architecture.md) 里“尾部保护”的表述在标点分支上并未实现。
  - 翻译模式规则：标点切点距当前 ASR 文本末尾不足 `guard_tail_chars` 个字符时**先不切**，等 ASR 继续延展、切点离开不稳定尾部后再切；`is_sentence_end` 不受此限制。理由是翻译和已播音频一样不可撤回，不能把尚未收敛的尾部标点/短片段送去翻译。
  - 实现：新增策略开关，**默认关闭**，现有 TTS 路径的行为与测试完全不变；只在翻译模式开启。`sentence` 粒度本身不切句，天然规避该问题。
  - 代价：每个标点最多多等一次 ASR 更新（约 100–300ms）；guard 值与延迟的平衡需实测（见第 14 节）。
- **sentence 粒度**：句子未结束时**不调用**分块发送逻辑，只在 `is_sentence_end` 时把整句交给翻译；TTS 也只在一句结束时收到一条请求。

### 6.2 翻译 worker 与句终屏障

翻译请求不能在会话主循环里 `await`：主循环是 `biased select`，打字输入优先，阻塞一次 HTTP 会让打字变慢。用独立 tokio task：

```rust
struct TranslationJob {
    generation: u64,      // 重置/打断时递增，旧结果全部丢弃
    seq: u64,             // 同一 generation 内的顺序号
    text: String,         // 源文片段（clause）或整句（sentence）
    is_sentence_end: bool // 该片段是否携带本句句终
}

struct TranslationDone {
    generation: u64,
    seq: u64,
    source_end: bool,
    text: Result<String, String>,
}
```

- worker 内部顺序处理（单 in-flight），结果按 `seq` 顺序回传，天然保持 TTS 顺序。
- 会话侧维护：`generation`、`pending_jobs`、`sentence_end_pending`、`translated_accum`、`sentence_id`。
- 收到结果且 generation 匹配：
  - 成功：追加到 `translated_accum`，组装 `TtsChunkRequest { text: 译文, is_sentence_end: source_end, voice, rate, language: target }` 发 TTS；`emit PipelineEvent::Translation`。
  - 失败：`emit Error`；若 `source_end` 则仍要收口（发空文本+句终），保证 TTS task 不悬挂。
- **句终屏障**：ASR 报句终但还有未返回的翻译任务时，不能立刻给 TTS 发 `finish-task`；等该句 pending 归零再收口。`sentence` 粒度下由唯一的整句任务携带句终，逻辑更简单。
- 空收口：ASR 句终但分块没有新文本时，pending 归零后补发 `空文本 + is_sentence_end`（沿用现有做法），同时 `emit Translation { is_sentence_end: true, .. }` 让字幕收口。
- 停止/重启/关窗：abort worker 并丢弃 `tx_mt`；3s 收尾超时可能截断最后一句翻译，与现有 TTS 收尾行为一致。

### 6.3 源语言与目标语言

- `SentenceLanguageState` 继续负责**源语言**判定与每句冻结（现有 `language_for_chunk` 语义保留）。
- TTS 的语言/音色改为固定目标语言：`voice.selection_for(target)` 得到 `(voice_id, rate)`（可在会话启动时算一次），`TtsChunkRequest.language = target`。
- MT 的 `source_lang` 用该句冻结的源语言（ISO 代码直接透传；判定失败时用 `auto`）；打字输入用 `tracker.decide(text)`（与现有 `typed_language` 一致）。
- 源语言 == 目标语言时跳过 MT，直接把源文当译文送 TTS，避免无意义翻译（仍照常显示单语字幕）。

### 6.4 打字输入

- `Typed` 分支中，先按现有规则收口正在进行的 ASR 句：
  - `generation += 1`，丢弃该句所有 pending 翻译结果；
  - 立即给 TTS 发 `空文本+句终` 收口旧 task（此时旧译文不再等待）；
  - 再为新打字内容走翻译 worker，成功后发 TTS 并 `emit PipelineEvent::TranslatedSentence`。
- 打字翻译同样经过 worker（复用超时/重试/错误处理），但用新的 generation，保证不会和旧 ASR 结果乱序。
- `MAX_TYPED_INPUT_CHARS = 500` 不变。

## 7. TTS 与音色

- 目标语言直接进 `TtsChunkRequest.language`，从而 `tts_language_hint` 与 `instruction_for` 按目标语言生效（例如目标 `ja` 会带“请讲日语。”）。
- 音色：`voice.selection_for(target)`。自定义音色有目标语言样本则用该样本，缺失时按现有行为回退默认语言样本；**不额外提示**（已确认）。
- 播放链路（22050 Hz PCM、重采样、FIFO）完全复用，不做双路混音。

## 8. 字幕模型与事件

### 8.1 数据模型

```rust
struct SubtitleLine {
    language: String,        // 目标语言代码
    text: String,            // 译文
    source: String,          // 原文
    source_language: String, // 源语言代码
    sentence_id: Option<u64>,
}

struct SubtitleState {
    translation_mode: bool,        // 会话启动时按配置设置
    current: String,               // 源文（ASR 持续修订）
    translated_source_bytes: usize,// 已送翻译的源文前缀（源文分色）
    current_translation: String,   // 当前句累计译文
    translation_sent_bytes: usize, // 已送 TTS 的译文前缀（译文分色）
    language: Option<String>,      // 目标语言
    source_language: Option<String>,
    history: Vec<SubtitleLine>,
    last_finalized_sentence_id: Option<u64>,
}
```

- 历史行必须有 `sentence_id`，否则晚到的译文无法回填（现有 `SubtitleLine` 只有 language/text）。
- 500 句上限、停止/重开只清当前句、历史按 `sentence_id` 去重等 D16 规则保持不变。
- 翻译模式下当前句句终由 `Translation.is_sentence_end` 触发收口；非翻译模式仍由 `Asr.is_sentence_end` 触发，行为不变。

### 8.2 事件（D6 要求同步 events.rs / app.rs / decisions / architecture）

```rust
PipelineEvent::Translation {
    text: String,           // 当前句累计译文
    sent_bytes: usize,      // 已送 TTS 的译文字节前缀
    is_sentence_end: bool,
    language: String,       // 目标语言
    sentence_id: Option<u64>,
}

PipelineEvent::TranslatedSentence {
    source: String,
    translation: String,
    source_language: String,
    target_language: String,
}
```

- 翻译模式下 `Asr` 事件仍用于更新源文与“已翻译源前缀”，但**不再负责句终收口**（`is_sentence_end` 恒为 false，或由 app 侧按模式忽略）。
- 非翻译模式不产生 `Translation` 事件，UI 行为完全不变。

### 8.3 UI（`src/gui/realtime_tab.rs`）

- 当前句：源文小字弱色（已翻译前缀正常色）、译文大字（已送 TTS 前缀正常色）。
- 历史行：译文大字 + 原文小字；**原文行与译文行各自有一个复制按钮**（悬停对应行时显示），分别复制原文/译文，避免“想复制原文却拿到译文”。
- 语言列显示目标语言代码；原文行前加小号源语言代码（如 `zh`）。
- `show_source = false` 时只渲染译文行（仍保留译文复制按钮），行高与复制逻辑相应调整。
- 当前句未收口时不可复制（与现有历史行行为一致）。

## 9. GUI 与热更新

- 新增独立 `Tab::Translate`（「翻译」页，`src/gui/translate_tab.rs`），所有翻译选项集中在该页；工具栏保持现状，只保留原有的开始/停止等控件。
  - `enabled` 开关、目标语言下拉（16 种，显示中文名）、`granularity` 二选一（标点即翻 / 整句再翻）、`show_source` 开关：GUI 可写，改值即走 `config_edit` 写回 `config.toml`，失败时走现有 `config_write_error` 提示并回滚显示。
  - `model` 只读展示（手改配置后「重新加载配置」生效）。
- 运行中修改 `enabled` / `target` / `granularity` / `model`：走现有 D20 指纹变化 → 停旧会话 → 用新配置自动重启；会丢当前句，页内注明。`show_source` 只影响渲染，不参与 `config_signature`，改后立即生效、不重启。
- 空闲时修改只影响下次开始。
- 未启用翻译时，目标语言/粒度控件禁用；`show_source` 仍可改。

## 10. 错误处理与边界

| 场景 | 行为 |
|---|---|
| MT 超时/失败 | Error 红条；跳过该段；句终仍收口；不杀会话 |
| MT 返回空串 | 视为失败处理，不送 TTS |
| 连续失败（断网） | 每段都报，可能刷屏；用现有错误条（后续可加“连续失败合并”优化） |
| ASR 修订已翻译文本 | 已翻译/已播内容不可撤回（与现有已送 TTS 语义一致），文档说明 |
| 句终与异步翻译竞争 | 句终屏障 + pending 计数，禁止提前 finish-task |
| 打字打断 ASR 句 | generation 递增 + 立即收口旧 TTS task，旧结果丢弃 |
| 停止/重启/关窗 | abort worker；3s 收尾可能截断尾句 |
| 目标语言音色缺失 | 现有回退行为，不提示 |
| RPM/TPM 限流 | 单会话按标点翻译远低于 60 RPM；429 退避重试一次 |
| 成本 | qwen-mt-flash 0.7/1.95 元每百万 token；每小时对话大致几分钱，可忽略 |

## 11. 测试计划

### 单元测试

- `qwen_mt`：请求 JSON（model/messages/translation_options）、响应解析、非 2xx 错误体、超时/重试分支（用 mock HTTP，参照 `src/voice/mock_http.rs`）。
- `chunker`：新的“标点/句终切 + 安全尾保护”策略（含：标点落在尾部 guard 内不切、文本延展后再切、句终忽略 guard）；ASR 修订、标点补齐场景仍正确；默认策略的现有测试不变。
- 翻译句终状态机（建议抽成纯结构）：句终屏障顺序、generation 丢弃、失败收口、pending 归零。
- `SubtitleState`：ASR 事件 + 翻译事件交错、句终收口、历史去重、500 上限、双语历史、打字翻译、`show_source=false` 的纯逻辑（若可分离）。
- `config`：解析、`target` 归一化、非法 `granularity`/目标语言、`config_signature` 随每个字段变化；沿用 `app.rs` 现有签名测试风格。
- `language`：`TRANSLATION_LANGUAGES` 合法性、显示名。

### 运行期（D14：egui_mcp 人工回归）

- zh→en、zh→ja 实说，检查双语字幕、目标语言发音、音色是否保留。
- 切换 `granularity` 对比延迟与译文质量（clause vs sentence）。
- 打字输入翻译（中英混排、500 字符上限）。
- 运行中开关翻译/改目标语言 → 自动重启且新配置生效。
- 自定义音色有/无目标语言样本两种表现（不提示）。
- 模拟 MT 失败（临时改错 model）→ 红条、会话继续、句终不悬挂。
- 停止/重启/关窗：无残留任务、无 TTS 悬挂、无崩溃。

## 12. 里程碑

### M1 核心链路打通

- `[translate]` 配置 + MT 客户端 + 翻译 worker + 会话接入（先做 `clause` 粒度）。
- TTS 使用目标语言与目标语言音色；句终屏障。
- 最小字幕：当前句显示译文（历史行可仍只存译文）。
- **完成标准**：zh→en 实时说话能听到英文语音、看到英文字幕；打字输入能翻译；MT 失败只报错不崩；现有功能与测试不回归。

**状态：已完成（2026-11-07）**

- 已实现：`[translate]` 配置与校验；`client/qwen_mt.rs`（`translation_options` 必带、429/5xx 重试一次、错误码透出）；`pipeline/translator.rs`（generation + seq、单 in-flight 保序、同语言跳过）；`ChunkPolicy`（`clause` 安全尾 / `sentence`）；session 接入（句终屏障、打字 bump generation、TTS 收口）；`PipelineEvent::Translation`；app 复用 `SubtitleState::apply_asr`。
- 测试：全量 **164 passed**（含 qwen_mt 6 项、chunker 安全尾 3 项、config/language、配置指纹）。
- 运行期验证：中文打字输入 → 英文字幕 + 目标语言 TTS 请求（zh→en 翻译链路）；ASR 英文语音 → 英文字幕（en→en 跳过 MT）。
- 未覆盖：ASR 中文语音的 zh→en 实际听感（依赖麦克风环境，需人工回归）；M1 无 GUI 开关，需手改配置后「重新加载配置」。
- 验证中发现 `config_signature` 漏 translate 字段，已补字段与测试。

### M2 双语字幕、粒度开关与打字收尾

- 双语 `SubtitleLine` + `sentence_id` 回填 + 双进度分色 + 原文/译文各自复制。
- `granularity = "sentence"` 实现；`clause` / `sentence` 都可用。
- 打字输入完整接入（generation、打断、历史行）。
- 错误/空收口/停止重启等边界补齐。
- **完成标准**：切换两种粒度均能稳定工作；历史行双语正确、去重正确；打字打断不再错序。

**状态：已完成（2026-11-07）**

- 已实现：`SubtitleLine` 增 `source / source_language / sentence_id`；`SubtitleState` 增 `translation_mode / current_translation / translation_sent_bytes / source_language` 与 `apply_source` / `apply_translation` 双语 reducer（句终落「原文 + 译文」、同 `sentence_id` 去重、空译文只清当前行）；翻译模式下 `Asr` 事件只表达原文进度（`is_sentence_end = false`），句终由 `Translation` 负责，译文全部失败也会补发句终收口；实时页改为「译文大字行 + 原文小字弱色行」，两行各自语言列与复制按钮，`show_source = false` 只渲染译文行；`show_source` 不进配置指纹，重新加载配置即时生效。
- 测试：全量 **169 passed**（新增 5 项双语字幕 reducer 测试）；`cargo fmt --check`、`cargo build --bins`、`git diff --check` 通过。
- 运行期验证：拼音输入 → 日文字幕（zh→ja，双语两行、语言列 zh/ja）；悬停原文行复制得到中文原文、悬停译文行复制得到日文译文；`show_source = false` 重新加载配置后原文行立即隐藏、会话不重启。
- 未覆盖：`sentence` 粒度的真实语音端到端听感（打字输入本身是整句，未覆盖流式句终时序）；`clause` 模式长句的视觉分块效果仍需真实语料回归。

### M3 界面、文档与回归

- 独立「翻译」Tab（开关/目标语言/粒度/显示原文）+ 热更新。
- 文档归位（见第 13 节）、删本文件前的收尾。
- 完整门禁：`cargo fmt --check`、`cargo build --bins`、`cargo test`、`git diff --check`；egui_mcp 回归清单走一遍。
- **完成标准**：门禁全绿；README 的翻译模式说明可直接交付给用户。

## 13. 文档更新清单

| 文档 | 更新内容 |
|---|---|
| `config.example.toml` | 新增 `[translate]` 段及逐字段注释（权威字段表） |
| `README.md` | 界面说明（翻译页：开关/目标语言/粒度/显示原文）、打字输入、配置摘要、16 种目标语言表、翻译模式使用与延迟预期 |
| `docs/architecture.md` | 新模块与数据流（翻译阶段、worker、字幕双轴）、配置字段表、事件表、UI 说明；并修正“尾部保护”只作用于兜底切分的表述 |
| `docs/decisions.md` | 新增 `D25`（翻译模式：ASR/打字 → MT → 现有 TTS，含分块与失败边界）；已补 `D26`（双语字幕与逐行复制、事件语义、`show_source` 热加载） |
| `docs/translation.md` | 本文件；功能完成后删除，长期内容已并入上述文档 |

## 14. 待实测/待确认

> 协议与端点已在 2026-11-07 的 spike 中确认（见 5.1），以下只列仍未实测的项。

1. ASR 流式结果里句中标点的出现频率——它直接决定 `clause` 粒度的实际延迟；若很少出现，clause 会退化成句终翻译，这也是保留开关的原因。标点安全尾的 `guard` 值（默认 3）与 ASR 更新节奏的平衡也要实测。
2. 端到端延迟实测：ASR 出字 → MT → TTS 首包 → 播放，目标控制在译文比原文晚 1s 左右（MT 实测只占 0.2–0.3s）。
3. 16 种目标语言的 TTS 发音与音色自然度（尤其自定义音色跨语种）。
4. 碎句翻译质量（无跨句上下文）：先接受，实测后再决定是否把上一句作为上下文拼进请求（注意 8192 输入上限与“上下文也会被翻译”的问题）。
