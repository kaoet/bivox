# VRChat OSC 聊天框字幕：调研与实现方案

> 调研日期：2026-11-08
> 状态：**未改主程序代码**。本文是实现前的调研与方案，不进入当前事实源；实施时应把结论合并进
> [`docs/decisions.md`](../decisions.md)（新增 `D33`）、[`docs/architecture.md`](../architecture.md)、
> [`README.md`](../../README.md) 与 [`config.example.toml`](../../config.example.toml)。
>
> **v5 修订（2026-11-08 UI 精简，按你的要求）**
> - 翻译模式未开启时，三种显示模式仍可编辑，但**不显示灰色提示**；
> - **去掉状态行**（不显示最近发送/字符数/等待窗口/错误）；
> - 「发送测试消息」**立即发送，不受 1 秒文本节流约束**；
> - 页面**不显示**“附近玩家可见”的隐私提示，也不显示 typing 说明；行为照旧，只是不写文案。
>
> **v4 修订（2026-11-08 持久性实测后）**
> - 聊天框在**最后一个文本包后约 30 秒自然消失**；空字符串可以**立即清空**（均已实测）；
> - 你已撤回“历史字幕维持 5 秒”的需求：v1 **不再主动 5 秒清空**，句终后保留最后文本，
>   等 VRChat 自然过期（约 30 秒）；显式清空只用于停止会话/关闭 OSC/关窗；
> - 设计中去掉 5 秒计时与 `last_finalized_at`，状态机更简单；
> - 配置仍为 `[vrchat.osc] enabled / port / display`，文本间隔固定 1 秒、typing 不占限流。

---

## 0. 结论摘要

1. **协议很简单**：向本机 UDP `127.0.0.1:9000` 发送 OSC 消息
   `/chatbox/input <string> <immediate:bool> <notification-sound:bool>` 即可。
   字幕场景固定 `immediate = true`，`notification-sound = false`（第 3 个 bool 必须显式传，
   官方缺省是 `true`，不传会响）。
2. **官方限制**：文本最多 **144 字符**、最多显示 **9 行**（含换行与自动换行）；换行可用。
   官方没有说超长是截断还是拒绝，**客户端必须自己截断**；截断时**保留句尾**，用前导 `…`
   代替被截掉的句头（句尾才是刚说到的内容）。
3. **限流实测结论：固定 1 秒**。
   - 900ms 跑 120 秒：**到后来会跳字**；
   - 1000ms 跑 120 秒、144 字符长文本、并且每秒发送一次 typing：**始终正常**；
   - UDP 错误数始终为 0（发送成功）；跳字是 VRChat 内部合并/丢弃，不是网络错误。
   因此实现按 **文本最小发送间隔 1000ms** 设计（不是社区建议的 1500–1600ms）。
4. **聊天框持久性（已实测）**：只发一条消息、之后不再发包，聊天框约 **30 秒**后自然消失；
   发送空字符串则**立即清空**。你已接受 30 秒自然消失，因此**取消 5 秒历史显示逻辑**：
   - 句终后保留最后文本，不主动清空，等 VRChat 自然过期；
   - 只在停止会话、关闭 OSC、关窗时显式发空字符串；
   - 这样也省掉 5 秒计时和“历史句重新发送”的状态。
5. **typing 不占用限流**：`/chatbox/typing` 是独立的 OSC 消息。实测 1000ms 文本 + 每秒 typing
   连续 120 秒正常，文本不跳字、指示器每秒切换；因此 typing 不挤占文本的 1 条/秒预算。
   设计上仍只在"当前句开始/结束"时发 typing，但这只是 UX/流量考虑，不是限流要求。
6. **Rust 依赖用 `rosc`**（0.11.4，MIT/Apache-2.0，2025-03-23，约 62 万下载）编码，
   配合同步 `std::net::UdpSocket` 发送即可；不需要 async，不需要专用 VRChat crate。
7. **发送端不要 bind 9000**：用 `bind("127.0.0.1:0")` 拿到临时源端口，只 `send_to`，避免和 VRChat 抢端口。
   一条消息一个 UDP 包，**不要用 OSC bundle**（有"bundle 内多条只执行第一条"的社区 bug 报告）。
8. **界面**：在「语音合成」和「翻译」之间插入「VRChat」页；OSC 默认关闭；可配端口（默认 9000）；
   三种显示模式只对翻译模式生效，非翻译模式固定显示 ASR 原文；加「发送测试消息」按钮。
9. **配置**：新增父段 `[vrchat]` 下的 `[vrchat.osc]`，字段 `enabled` / `port` / `display`。
   **整个 `[vrchat.*]` 不进配置指纹**，改设置不重启实时管线；以后加 `[vrchat.avatar]` 等不需要动 OSC 逻辑。
10. **typing 指示器**：`/chatbox/typing true` 只在**当前句活跃**时打开，句终/清空时关闭；
    只在状态跳变时发送。实测表明它不占文本限流，因此可以和文本更新独立发送。
11. **架构建议**：不要只靠 egui 每帧更新来发 OSC（窗口最小化/被遮挡时可能不重绘，字幕会停止）。
    推荐把 OSC 做成一个**独立后台任务**，由会话把 `PipelineEvent` 克隆一份直接喂给它；
    OSC 任务自己维护字幕 reducer、1 秒文本节流、typing 和清空逻辑，与 GUI 是否重绘无关。
12. **调试**：大部分开发不需要打开 VRChat（本地 UDP 模拟接收端即可）；最终验收需要真机。
    本机 VRChat（2026.4.1p1）实测已验证 1 秒、30 秒自然过期与空串清空；正式实现后做一次
    10 分钟浸泡即可。

---

## 1. 需求与现有代码的接入点

### 1.1 需求回顾

| 编号 | 需求 | 关键点 |
|---|---|---|
| R1 | 「翻译」页左侧新增「VRChat」页 | 页签顺序：实时 / 音频 / 语音识别 / 语音合成 / **VRChat** / 翻译 / 设置 |
| R2 | 可开关 OSC（默认关闭） | 关闭时不发任何包；开启后持续更新聊天框 |
| R3 | 三种显示模式 | 只显示原文 / 原文和译文 / 只显示译文；**只对翻译模式生效** |
| R4 | 端口可配 | 默认 9000；VRChat 启动参数改端口时同步修改 |
| R5 | 配置写入 `[vrchat.osc]` | `[vrchat]` 作为父段，给以后其它 VRChat 功能留位置 |
| R6 | 字幕内容语义 | 有当前句显示当前句（ASR 最新文本，可未稳定）；没有当前句时**保留最后文本**，由 VRChat 约 30 秒后自然消失（**v4 起取消原 5 秒强制清空**） |
| R7 | typing 区分当前/历史 | 当前句活跃时 `typing=true`；显示历史/清空时 `typing=false` |
| R8 | 发送测试消息 | VRChat 页提供按钮，不依赖麦克风/API 验证链路 |

### 1.2 现有实现里与本功能直接相关的部分

| 位置 | 事实 | 对本功能的意义 |
|---|---|---|
| [`src/app.rs:244`](../../src/app.rs#L244) `Tab` | 固定 6 个页签，`ALL: [Tab; 6]` | 加第 7 个 `Tab::Vrchat`，插在 `Tts` 与 `Translate` 之间 |
| [`src/app.rs:1129`](../../src/app.rs#L1129) | `match self.tab` 逐个绘制 | 新增 `Tab::Vrchat => gui::vrchat_tab::show(...)` |
| [`src/app.rs:55`](../../src/app.rs#L55) `SubtitleState` | 已维护：普通模式 `current`、翻译模式 `current`(原文) + `current_translation`、`history`、`sentence_id` 去重 | **这是聊天框文本的唯一正确来源**，不要另写一套 ASR 文本状态 |
| [`src/app.rs:133`](../../src/app.rs#L133) `apply_asr` / `apply_source` / `apply_translation` | 普通/翻译两种事件语义（D26） | 聊天框必须按同一语义归约事件，否则原文/译文会错位 |
| [`src/events.rs:54`](../../src/events.rs#L54) `PipelineEvent::Asr` | 翻译模式下只刷新源文进度，`is_sentence_end` 恒 false | 翻译模式下当前句的"原文"来自它 |
| [`src/events.rs:65`](../../src/events.rs#L65) `PipelineEvent::Translation` | 译文累积、句终收口 | 翻译模式下"译文"和句终来自它 |
| [`src/events.rs:76`](../../src/events.rs#L76) `TypedSentence` | 打字输入提交即进历史 | 没有当前 ASR 句时，打字句作为最后文本保留，约 30 秒后随聊天框自然消失 |
| [`src/config.rs:85`](../../src/config.rs#L85) `AppConfig` | `deny_unknown_fields` + 每段默认值 | 新增 `VrchatConfig { osc: VrchatOscConfig }`，默认关闭/9000/bilingual |
| [`src/config_edit.rs:56`](../../src/config_edit.rs#L56) `set_translate` | `toml_edit` 定点写回、写前校验、`.bak` | 新增 `set_vrchat_osc`，照同一模式实现 |
| [`src/app.rs:1169`](../../src/app.rs#L1169) `config_signature` | 手工拼运行时字段；`show_source` 刻意不入指纹 | `[vrchat.*]` 也**不入**指纹，见 4.1 |
| [`src/pipeline/session.rs:118`](../../src/pipeline/session.rs#L118) `emit` | 所有后台事件都从这里发往 GUI | 在这里把事件**克隆一份**发给 OSC 服务 |
| [`src/pipeline/session.rs:294`](../../src/pipeline/session.rs#L294) | 会话进入 `Running` 时发 `StateChanged(Running)` | OSC 服务据此开始新会话状态 |
| [`src/app.rs:866`](../../src/app.rs#L866) 启动包装任务 | 出错时向 GUI 发 `Error` + `Failed` | `Failed` 也要发给 OSC 服务，否则聊天框会停在最后一句 |
| [`src/gui/translate_tab.rs`](../../src/gui/translate_tab.rs) | 翻译页的"改值即保存"模式 | VRChat 页沿用同样的交互与写回流程 |

### 1.3 现有字幕状态的关键语义（必须对齐）

- 普通模式：`current` 是 ASR 当前句；句终且非空时落进 `history`，同一 `sentence_id` 只记一次。
- 翻译模式：`current` 是源文，`current_translation` 是累计译文；句终由 `Translation` 事件负责，
  `Translation { is_sentence_end: true }` 时把「译文 + 原文」落进历史；空译文句终只清当前行、不落历史（D26）。
- 历史保留 500 句，停止/重启只清当前句、历史保留（D16）。
- `show_source` 只控制 GUI 是否显示原文行，**不应**影响 `[vrchat.osc]` 的三种显示模式（两套独立设置）。

---

## 2. VRChat OSC 协议调研

### 2.1 官方事实（2026-11 仍有效）

官方 ChatBox 章节不在独立页，而在 **OSC as Input Controller** 页底部（该页 `updatedAt: 2025-11-13`）：

- 文档：<https://docs.vrchat.com/docs/osc-as-input-controller#chatbox>
- OSC 总览（端口/启用）：<https://docs.vrchat.com/docs/osc-overview>
- 启动参数：<https://docs.vrchat.com/docs/launch-options>
- OSC 调试：<https://docs.vrchat.com/docs/osc-debugging>
- OSCQuery：<https://docs.vrchat.com/docs/oscquery>

官方原文要点：

```
/chatbox/input s b n
  s = 文本
  b = true：立即发送、绕过键盘；false：打开键盘并填入文本（不会直接发送）
  n = false：不播放通知音；不传时默认 true（会响）

/chatbox/typing b
  true/false 切换"正在输入"指示器

限制：144 characters；最多 9 行（including new lines and word wrap）
```

| 项目 | 结论 |
|---|---|
| 接收端口 | **9000**（VRChat 收）；发送端口 9001（VRChat 发，本功能不需要） |
| 目标地址 | `127.0.0.1`（官方默认）；本功能固定 loopback |
| 改端口 | 启动参数 **`--osc=inPort:senderIP:outPort`**，默认等价 `--osc=9000:127.0.0.1:9001`；官方只记录三段式，`--osc=9000` 单值简写未文档化，不要依赖 |
| 启用 OSC | Action Menu → **Options → OSC → Enabled**；打开 OSC 区的 **OSC Debug** 也会顺带开启 OSC |
| 文本格式 | 纯文本 + 换行；不支持富文本/颜色（社区结论） |
| typing | `/chatbox/typing` 存在；**v1 使用**，用于区分当前句/历史句（见 4.3.4） |
| OSCQuery | VRChat 2023.3.1+ 支持，mDNS 自动发现；**v1 不做**，手动端口足够（见 5.3） |

### 2.2 社区限流资料（作为背景，不是最终结论）

**官方文档没有写速率限制。** 社区证据：

1. **VRCOSC**（活跃维护的 VRChat OSC 应用）
   - 设置项 `ChatBoxSendInterval` 默认 **1500ms**；
   - FAQ 原文："VRChat has a default minimum of 1500 milliseconds. If it still occurs, keep increasing
     the value by 100 until the timeout stops."
   - 来源：<https://vrcosc.com/docs/faq>（ChatBox Issues → Timed out for spam），
     源码：<https://github.com/VolcanicArts/VRCOSC>
2. **VRC-OSC 的 ChatBox 插件**
   - README 原文："1.5 seconds is the quickest you can send a chatbox message without being timed out
     for spamming"
   - 来源：<https://github.com/ShayBox/VRC-OSC>（`plugin-chatbox/README.md`）
3. 另有社区文档称"5 条 / 5 秒，超过触发 30 秒冷却"（TTS Voice Wizard）。

这些数字说明"VRChat 聊天框有秒级限制"，但**具体值因版本/机器/文本长度而异**；本次实测
（2.3）得到的 1 秒与它们不一致，以实测为准。

### 2.3 本机实测与最终结论（2026-11-08）

#### 测试条件

- VRChat `2026.4.1p1`，桌面模式，私人/朋友实例，`OSC enabled` 已在 Action Menu 打开，
  `Show Own ChatBox` 打开；
- 用一个临时 Rust 工具直接发 `/chatbox/input`（`immediate=true`、`notification-sound=false`），
  不经过 kaols；该工具已完成使命并删除，数据记录如下；
- 发送计数唯一（`#0001`…），人工观察聊天框是否跳号/卡住；
- 终端错误数 = UDP `send_to` 失败数。

#### 结果

| 测试 | 结果 |
|---|---|
| 900ms，短文本，20 秒 | 无跳字（早期短测） |
| 850ms，短文本，20 秒 | 约 24 条中跳 1–2 条 |
| **900ms，短文本，120 秒** | **到后来会跳字**（短测的"无跳字"不能外推） |
| **1000ms，144 字符长文本 + 每秒 typing，120 秒** | **始终正常，无跳字；typing 指示器每秒切换** |
| UDP 错误 | 全程 **0** |
| `Timed out for spam` | 测试中未观察到（900ms 表现为跳字，不是 UDP 错误） |

#### 持久性 / 清空实测（2026-11-08）

- **自然过期**：20:59:15.458 发送 `PERSIST-B ... WATCH-UNTIL-IT-DISAPPEARS`，之后**不再发任何包**；
  聊天框在 **20:59:45 左右消失**，约 **30 秒**。
- **显式清空**：21:01:29.478 发送 `CLEAR-TEST-2 ... WATCH-FOR-CLEAR`；
  21:01:34.714（约 5.2 秒后）发送空字符串 + `typing=false`；
  聊天框**在 21:01:34.7 左右立刻消失**，没有等 30 秒。
- 结论：
  1. VRChat 聊天框**不是靠持续发包维持**的，最后一个文本包之后约 30 秒自然过期；
  2. 空字符串可以**立即清空**（显式清空有效）；
  3. 30 秒 > 原来的 5 秒需求，所以如果坚持 5 秒必须显式清空；**你已撤回 5 秒需求并接受 30 秒自然消失**，
     因此 v1 不再做 5 秒计时，句终后保留最后文本即可；
  4. 显式清空保留给：**停止会话、关闭 OSC、关窗**（以及句终后没有任何可显示文本的极端情况）。
- 局限：30 秒只测了一次，具体值可能随版本/场景变化；它只是**兜底**，不应当作精确计时器。

#### 结论

1. **文本最小间隔固定 1000ms**。900ms 长跑不稳定，1000ms 在"长文本 + typing"压力下稳定；
   这比社区 1500ms 更激进，但有本机 2 分钟实测支撑。
2. **typing 不占用文本的 1 条/秒限流预算**：1000ms 文本 + 每秒 typing（合计约 2 条 OSC/秒）
   连续 120 秒正常，说明 typing 消息没有挤掉文本更新。
3. **发送成功 ≠ 显示成功**：UDP 错误 0 只是 `send_to` 成功；900ms 的跳字是 VRChat 内部
   合并/丢弃，所以必须在真机看计数。
4. **边界与余量**：1000ms 是"当前机器/当前版本/当前文本长度"下稳定的值，不是理论保证。
   正式实现后仍建议做一次 10 分钟浸泡；如果未来 VRChat 版本/机器变化导致跳字，
   再把间隔调到 1050–1100ms。
5. **测试局限**：只测了短文本与 144 字符拉丁/中文填充、2 分钟、桌面模式、私人/朋友实例；
   没有覆盖 emoji、9 行以上、不同世界/网络条件、数小时连续运行。这些作为验收项。

> 之前设想的"v1 不预设限流、`min_interval_ms = 0`、先测再定"已经完成使命：
> **结论就是固定 1 秒**，因此不再保留实验性配置字段。

### 2.4 文本、Unicode、换行、清空

| 问题 | 结论 | 依据/风险 |
|---|---|---|
| 144 的长度单位 | 官方只说 characters；**未说明** Unicode 标量、UTF-16 码元还是显示宽度 | 中文 1 字通常 1 个 UTF-16 码元；emoji 可能 2 个。实现按 **UTF-16 码元预算 144** 最保守，且不切断 `char` 边界 |
| 超长行为 | 官方未写截断/拒绝/报错 | 客户端必须自己截断；不要依赖 VRChat 处理 |
| 换行 | 支持；9 行上限含换行与自动换行 | 官方原文；两条字幕建议用 `\n` 分两行 |
| 中文/日文 | 官方当前文档已不再写 "ASCII only"（旧 wiki 的 ASCII 说法已过时）；社区 2024-12 实测日文 chatbox 正常显示 | 旧 wiki：<https://github.com/vrchat-community/osc/wiki/Chatbox>；日文实测：<https://zenn.dev/kx_ras/articles/681cf6df73430b>；中文/emoji 仍需真机确认 |
| 清空 | 发空字符串 `""` 可**立即清空**（2026-11-08 实测：21:01:34.7 发送后立即消失） | 官方未明说；已实测有效；若未来失效，退化为发送单个空格并接受"空框" |
| 富文本 | 不支持 `<color>` 等 | 纯文本 |

**9 行上限的实践含义**：我们的双语最多 2 行，正常不会超；但打字输入可能包含多行（Shift+Enter），
合成文本时要按行截断到 9 行，并做 144 字符/码元截断。

### 2.5 本机环境

- VRChat 安装于 `D:\SteamLibrary\steamapps\common\VRChat`；
- 本机日志（`%USERPROFILE%\AppData\LocalLow\VRChat\VRChat\output_log_2026-11-08_*.txt`）显示：
  - OSC 服务在 9000、OSCQuery 动态端口；
  - 启动时 `OSC enabled: False`，实测前已在 Action Menu 手动打开（日志只在启动时打印该状态）；
- VRChat 构建：`2026.4.1p1-1937-...`，Unity `6000.0.67f1`；桌面模式（`XR Device: None`）。

结论：**VRChat 端不需要改 OSC 端口**，默认 9000 即可；真机测试需要打开 OSC 与
`Show Own ChatBox`（VRCOSC FAQ 提到这个设置，否则看不到自己的聊天框）。

### 2.6 仍需正式验收时确认的点

1. 144 的精确计数单位，以及中文/日文/emoji 的实际上限；VRChat 是截断、丢弃还是原样接受超长。
2. 30 秒自然过期只测了一次；不同世界/版本下的具体时长可能变化（它只作兜底，不精确依赖）。
3. 1 秒间隔在数小时连续运行、不同世界/网络条件下的稳定性（正式实现后 10 分钟浸泡 + 实际使用）。
4. `Show Own ChatBox` 的准确设置路径与桌面模式下的可见性。

---

## 3. Rust OSC 库调研与选型

### 3.1 候选对比（crates.io，2026-11-08）

| crate | 最新版 | 发布日期 | 总下载 | License | 结论 |
|---|---|---|---|---|---|
| **`rosc`** | **0.11.4** | 2025-03-23 | 622,092 | MIT/Apache-2.0 | **推荐**：纯 Rust OSC 1.0，message + bundle，依赖仅 `byteorder` + `nom`；API 稳定，足够用 |
| `osc` | 0.1.0 | 2023-11-16 | 1,795 | MIT/Apache-2.0 | 功能极少的空壳，不建议 |
| `nannou_osc` | 0.20.0 | 2026-06-22 | 41,287 | MIT/Apache-2.0 | 绑 nannou 生态，依赖重，不划算 |
| `vrchat_osc` | 2.2.0 | 2026-02-05 | 4,908 | MIT/Apache-2.0 | VRChat 专用但年轻，拖入 tokio/axum/OSCQuery；本功能用不上 |
| `vrchat-box` | 1.1.0 | 2025-03-12 | 791 | non-standard | CLI 工具，基于 rosc，可作参考实现；不作为依赖 |

- `rosc`：<https://crates.io/crates/rosc>、<https://docs.rs/rosc/0.11.4/rosc/>
- `vrchat_osc`：<https://crates.io/crates/vrchat_osc>

### 3.2 推荐用法

`rosc` 只负责编码；发送用 `std::net::UdpSocket`。对本项目（每秒最多 1 条文本 + 少量 typing）完全够用，
**不建议**引入 async/tokio 网络栈，也不建议引入 OSCQuery 依赖。

```rust
use rosc::{encoder, OscMessage, OscPacket, OscType};

fn encode_chatbox(text: &str) -> Result<Vec<u8>, rosc::OscError> {
    let message = OscMessage {
        addr: "/chatbox/input".to_string(),
        args: vec![
            OscType::String(text.to_string()),
            OscType::Bool(true),  // immediate：绕过键盘直接显示
            OscType::Bool(false), // notification sound：不响
        ],
    };
    encoder::encode(&OscPacket::Message(message))
}

fn encode_typing(typing: bool) -> Result<Vec<u8>, rosc::OscError> {
    let message = OscMessage {
        addr: "/chatbox/typing".to_string(),
        args: vec![OscType::Bool(typing)],
    };
    encoder::encode(&OscPacket::Message(message))
}

// socket = UdpSocket::bind("127.0.0.1:0")?;
// socket.set_nonblocking(true)?;
// socket.send_to(&encode_chatbox("...")?, ("127.0.0.1", port))?;
```

要点：

- **一条消息一个 UDP 包**，不要 bundle（社区有 bundle 多条只执行第一条的 bug 报告）。
- 发给 `127.0.0.1:port`；socket 绑定 `127.0.0.1:0`（临时源端口）。
- `send_to` 失败不代表 VRChat 不在运行（UDP 无连接）；失败只记日志，不弹错误横幅。
- 文本发送间隔由 4.4 的 1 秒常量控制；`rosc` 本身没有速率限制。

---

## 4. 功能设计

### 4.1 配置：`[vrchat.osc]`

```toml
# VRChat 相关功能。父段 [vrchat] 预留给以后的 avatar/参数等功能，
# 当前只有 OSC 聊天框，所以实际只写 [vrchat.osc]。
[vrchat.osc]
# 是否把实时字幕发送到 VRChat 的 OSC 聊天框（默认关闭）。
# 使用前需在 VRChat 中开启 OSC：Action Menu → Options → OSC → Enabled。
# 发送地址固定为本机 127.0.0.1；端口需与 VRChat 的 OSC 接收端口一致（默认 9000）。
enabled = false

# VRChat OSC 接收端口。若用启动参数改过（--osc=inPort:senderIP:outPort），这里要填相同的 inPort。
port = 9000

# 翻译模式下的显示内容（仅翻译模式生效；非翻译模式始终显示 ASR 原文）：
#   "source"      = 只显示原文
#   "bilingual"   = 显示原文和译文（译文在前、原文在后，两行）
#   "translation" = 只显示译文
display = "bilingual"
```

> **文本发送间隔固定 1 秒**（实测结论，见 2.3），由实现中的常量控制，不提供配置项；
> 以后若 VRChat 版本变化需要调整，再加配置字段也来得及。

对应 Rust 模型（放在 `src/config.rs`，与 `TranslateConfig` 同级）：

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VrchatConfig {
    pub osc: VrchatOscConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VrchatOscConfig {
    pub enabled: bool,
    pub port: u16,
    pub display: VrchatDisplay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VrchatDisplay {
    Source,
    #[default]
    Bilingual,
    Translation,
}
```

- 默认值：`enabled = false`、`port = 9000`、`display = Bilingual`。
- 校验：`port != 0`（`u16` 已经保证 ≤65535）。
- 写回：`config_edit::set_vrchat_osc(original, enabled, port, display)`，只改 `[vrchat.osc]` 的三个键，
  **保留**其它 `[vrchat.*]` 子表、注释与顺序；沿用"写前校验 → `.bak` → 写盘"。
  需要新增 `ensure_nested_table(doc, "vrchat", "osc")` 一类的 `toml_edit` 辅助（现有 `ensure_table` 只处理单层）。
- **D3 白名单扩展**：`[vrchat.osc] enabled/port/display` 加入 GUI 可写字段。
- **D20 配置指纹**：整个 `[vrchat.*]` **不加入** `config_signature`。聊天框只是旁路文本输出，
  改开关/端口/模式不应重启 ASR/TTS；运行中修改由 OSC 服务实时读取新配置生效。
- **父段语义**：`[vrchat]` 目前只有 `osc` 子段；以后新增 `[vrchat.avatar]` 等，
  不会影响 `[vrchat.osc]` 的解析、写回与配置指纹。

> 注意：`[translate] show_source` 只管 GUI 字幕；`[vrchat.osc] display` 管聊天框，两者独立。

### 4.2 界面：新增「VRChat」页

页签顺序（D32 的顺序扩展）：

```text
实时 / 音频 / 语音识别 / 语音合成 / VRChat / 翻译 / 设置
```

`Tab::ALL` 从 `[Tab; 6]` 改成 `[Tab; 7]`；`Tab::Vrchat` 插在 `Tts` 与 `Translate` 之间；
`central()` 的 `match` 增加 `Tab::Vrchat => gui::vrchat_tab::show(self, ui)`。

页面布局建议：

```text
VRChat
  把实时字幕发送到 VRChat 的 OSC 聊天框。需要 VRChat 中开启 OSC（Action Menu → Options → OSC → Enabled）。

  [ ] 启用 OSC 聊天框（默认关闭）

  发送端口  [ 9000 ]    （1–65535；VRChat 默认 9000）

  显示模式
  ( ) 只显示原文
  (•) 显示原文和译文
  ( ) 只显示译文

  [发送测试消息]
```

交互细节：

- **开关、模式：改值即保存**（与翻译页一致）：勾选/切换后走 `config_edit::set_vrchat_osc` + `reload_config`；
  由于 `[vrchat.*]` 不在指纹里，不会重启会话。
- **端口**：用 `egui::DragValue`（范围 1..=65535）或文本框；只在 `drag_stopped()` / 失去焦点 /
  回车时写回，避免拖动时每帧写盘。非法值（0）在本地拦截并显示页内红字，不写盘。
- **模式控件**：翻译模式关闭时，三种模式仍可编辑（方便预先配置）；**不显示任何灰色提示**。
- **发送测试消息**：通过同一 OSC 服务发一条固定文本（如 `kaols OSC 测试：你好，VRChat！`），
  不依赖麦克风/API；**立即发送，不受 1 秒文本节流约束**，也不改变 typing 状态。
  若 OSC 未启用则按钮禁用。会话运行中点击时，测试文本可能在下一次当前句更新时被覆盖；
  这是预期行为。
- **不显示状态行**：不展示最近发送时间/字符数/等待窗口/错误；发送失败只写日志。
- **不显示 typing 说明**：行为照旧（当前句活跃时自动打开，句终/清空时关闭），但页面不写提示。
- **不显示隐私提示**：页面不放“附近玩家可见”之类的文案。
- 若配置无效：与其它页一致，显示"配置无效，无法保存"。

### 4.3 聊天框文本合成规则

#### 4.3.1 显示模式与内容

| 翻译模式 | `[vrchat.osc] display` | 当前句内容 | 当前句结束后的保留内容（约 30s 自然消失） |
|---|---|---|---|
| 关闭 | 任意（忽略） | ASR 最新原文 `current` | 最后一条历史句的 `text`（普通模式 = ASR 文本） |
| 开启 | `source` | 源文 `current` | 最后一条历史句的 `source` |
| 开启 | `bilingual` | 译文 `current_translation` + `\n` + 源文 `current`（缺一就只发有的） | 最后一条历史句的 `text`(译文) + `\n` + `source` |
| 开启 | `translation` | 译文 `current_translation`；**为空时本次不更新**（保留聊天框上一帧内容，不乱显示原文） | 最后一条历史句的 `text`(译文)；为空则跳过 |

说明：

- **双语顺序：译文在上、原文在下**（已确认）。聊天框主要是给他人看的，译文是主要信息；
  同时与 GUI "译文大字行 + 原文小字行"一致。
- `bilingual` 的当前句只有源文、译文还没回来时：只发源文。
- `translation` 的当前句译文还没回来时：**不发**，聊天框保留上一帧；等译文到达后再更新。
- 非翻译模式完全忽略 `display`（需求 2）。
- 打字输入：
  - 普通模式：`TypedSentence` 已进历史；当前 ASR 句为空时，作为最后文本保留，约 30 秒后自然消失；
  - 翻译模式：打字句走源文 `Asr` + 译文 `Translation`，句终落历史后同样保留最后文本；
  - 有当前 ASR 句时，当前句优先；打字句在历史里等当前句结束后才可能显示。
- 停止/失败/关闭 OSC：显式清空（发 `""`）并把 typing 设为 false；正常句终**不**主动清空。

#### 4.3.2 保留与清空（30 秒自然过期）

VRChat 聊天框在**最后一个文本包后约 30 秒自然消失**（实测），因此不需要 5 秒计时：

```text
session_active = false                    -> 显式清空 + typing=false
current_source / current_translation 非空 -> 按模式显示"当前句" + typing=true
当前句刚结束（历史里有最后一条）          -> 发送最后文本（如与已发内容不同）+ typing=false；
                                             之后不发，等 VRChat 约 30 秒自然过期
没有当前句、也没有新内容                  -> 不发（已显示的文本继续计时；30 秒后自然消失）
停止/失败/关闭 OSC/关窗                   -> 显式空字符串 + typing=false（立即消失）
```

- 句终时要把**最终文本**发出去（可能比上一个 1 秒窗口多了最后几个字），然后停止发送；
- 不再维护 `last_finalized_at`，也不做“5 秒后清空”；
- 自然过期只是兜底：它保证程序异常退出时聊天框不会永久残留；
- 如果句终后没有任何可显示文本（例如"只显示译文"但整句译文为空），可以显式清空，
  避免上一个句子的旧译文继续挂 30 秒；
- 停止会话/关闭 OSC/关窗必须显式清空（已实测空字符串立即生效）。

#### 4.3.3 截断与换行（保留句尾）

```text
1. 统一换行：\r\n / \r -> \n；去掉每行首尾空白；折叠连续空行
2. 行数：最多 9 行（官方限制）
   - 普通/单行文本：超过 9 行时保留最后 9 行（丢句头）
   - 双语：优先保留译文，再用剩余行数保留源文尾部（译文行多时只保留译文尾部）
3. 长度：按 UTF-16 码元预算 144（含 \n 与省略号）
   - 超长时保留句尾，把被截掉的句头替换为前导 "…"（不是末尾加 "…"）
   - 被截断的每一行都在行首加 "…"，且 "…" 计入 144 预算
4. 双语预算分配：
   - 译文优先：先取译文尾部（最多 144）；译文自己就超长时只显示译文尾部，省略源文
   - 若还有剩余预算：加 "\n" + 源文尾部（源文截断同样保留句尾、前导 "…"）
   - 源文尾部是"刚刚在说的内容"，所以源文绝不能保留句头、丢掉句尾
```

示例（`…` 表示被截掉的句头）：

```text
source 模式：
  …这是很长很长的一句话的最后部分

bilingual 模式：
  …译文很长时的尾部
  …源文很长时的尾部
```

- 为什么保留句尾：断句时最旧的信息在句头，最新、最可能在继续的信息在句尾；用户看到 `…最后几个字`
  比看到 `很早的几个字…` 更有用。
- 长度预算按 **UTF-16 码元**：VRChat 是 Unity/C#，`string.Length` 是 UTF-16 码元数；中文 1 字 = 1，
  emoji 通常 2。按这个预算比按 Rust `chars()` 更保守；末尾加省略号的旧方案已废弃。
- 截断点必须在 `char` 边界（UTF-16 预算下还要避免拆开代理对），避免切坏多字节字符。
- 官方说 9 行含自动换行；我们无法精确预测聊天框宽度导致的实际换行，144 字符预算通常已经足够。
- 如果真机发现中文在远少于 144 字时就被截断，说明 VRChat 可能按字节或显示宽度限制；
  改为按 144 字节预算（约 48 个汉字）或按实测上限收紧，并在 README 记录。
- 截断/换行函数是纯函数，单测覆盖：保尾、前导 `…`、9 行保尾、双语预算、中文/emoji 边界。

#### 4.3.4 typing 指示器（当前句 vs 历史句）

VRChat 的 `/chatbox/typing` 显示"正在输入"指示器；我们用它表达**当前句正在活跃**：

| 状态 | 聊天框文本 | typing |
|---|---|---|
| 当前句存在（`current` 或 `current_translation` 非空，且会话 Running） | 当前句（按 display 模式） | **true** |
| 当前句已句终，聊天框保留最后文本（约 30s 自然消失） | 最后文本 | **false** |
| 无当前句、无历史、已清空、停止/失败/关闭 OSC | 空 | **false** |

- **只在状态跳变时发送** `/chatbox/typing`，不要随每次文本更新重复发。
- 进入当前句：先发 `typing=true`，随后发文本（或同一批先 typing 后 text）；
  句终：先发最终文本（如有），再发 `typing=false`；
  清空：发 `""`，然后 `typing=false`。
- 当前句存在但"只显示译文"的译文还没到：`typing=true`，文本保持上一帧。
- 非翻译模式、翻译模式、打字输入都适用；typing 只跟"当前句是否存在"有关，与 display 模式无关。
- **限流关系（已实测）**：typing 不占用文本的 1 条/秒预算。1000ms 文本 + 每秒 typing 连续 120 秒正常，
  文本不跳字，指示器每秒切换。因此 typing 可以在状态跳变时独立发送，不需要挤占文本窗口。
  - v1 仍只在跳变时发（每句最多 2 条），减少无意义消息；
  - 若以后 VRChat 版本变化导致 typing 也占限流，再把它纳入预算/加开关。
- 停止/关闭 OSC/关窗：尽力发一次 `typing=false`；若 VRChat 不在运行，UDP 发送失败可忽略。

### 4.4 更新循环：文本固定 1 秒，typing 独立

- **文本最小间隔 1000ms**（实现常量 `CHATBOX_TEXT_INTERVAL = Duration::from_secs(1)`）。
  1 秒内收到多次文本变化时，只在下一个窗口发送**最新**内容；相同文本去重。
- **去重只对“同一段显示”有效**：当当前句从“无”变为“有”，或收到 `TypedSentence` 时，
  清掉“上次发送文本”的记忆并允许发送一次；否则上一句自然过期后，连续两次说同样的话
  可能会因为文本相同而不显示。这个“新显示段”重置不等待 30 秒，下一段总是能重新显示。
- **typing** 在 true/false 跳变时立即发送，不占文本窗口（见 4.3.4）。
- **事件驱动 + 200ms tick**：
  - 收到 `Asr` / `Translation` / `TypedSentence` 后更新状态，计算期望文本；
  - 若距上次文本发送 ≥1s 且内容变化，立即发送；否则等待；
  - 200ms tick 只负责 pending 清空、typing 状态检查、配置变化；**不等 5 秒、不做历史过期**。
- **停止/失败/关闭 OSC**：置清空意图，在下一个文本窗口发 `""`；关窗时可直接发一次（最多多一条，风险可接受）。
- **测试消息**：立即发送一条固定文本，不进入 1 秒文本窗口；不改变 typing 状态。
- **日志**：记录每次实际发送的时间、字符数、kind（text/typing/clear）、是否被去重/是否在等待窗口；
  用 `debug!`，不刷屏。不做界面状态行。
- **发送顺序**：进入当前句时先 typing=true 再文本；句终时先发最终文本（如有）再 typing=false；
  清空时 `""` 后 typing=false。typing 和文本在同一次窗口内发送也没有问题（实测 2 条/秒稳定）。
- **正常句终不主动清空**：保留最后文本，由 VRChat 约 30 秒自然过期；显式清空只用于停止/关闭/异常。

伪代码：

```text
on_event(event):
    state.apply(event)
    desired = compose(now)          // 当前句，或句终后的最后文本；没有内容则 Keep
    maybe_send_text(desired)

tick every 200ms:
    // 只处理 pending 清空、typing、配置变化；不做 5 秒过期
    if pending_clear: send_clear_when_due()
    update_typing()

maybe_send_text(desired):
    if !enabled || !session_active:
        pending_clear = true
        if typing: send_typing(false)
        return
    match desired {
        Keep => {}
        Text(text) => {
            if text == last_sent_text: return
            if now - last_text_sent_at < 1000ms: return
            send_text(text); last_sent_text = text; last_text_sent_at = now
        }
        Clear => send_clear_when_due(),
    }
```

- 文本实际频率最高 1 条/秒；
- typing 独立，不更新 `last_text_sent_at`；
- 正常句终只是不再发送，**不**清空；VRChat 约 30 秒自然过期；
- 停止/关闭/异常才置 `pending_clear`，显式清空；
- 若某次 `send_to` 失败，只记日志；下个窗口再试。

`compose` 的三种结果：`Text`（当前句或句终后的最后文本）、`Keep`（没有新内容，聊天框保持现状）、
`Clear`（只在停止/关闭/异常时产生）。正常句终没有 `Clear`。

### 4.5 运行时架构：独立后台 OSC 服务（推荐）

#### 4.5.1 为什么不直接在 UI 每帧发

在 `KaolsApp::ui()` 里根据 `SubtitleState` 发 UDP 最直观，但 VRChat 通常全屏/独占，
kaols 窗口可能最小化或被遮挡；eframe 在窗口不可见时**不保证继续调用 `ui()`**，而
`PipelineEvent` 目前也是在 `ui()` 的 `pump_events()` 里消费的。这样会导致：

- ASR/TTS 后台还在跑，但 GUI 事件泵停摆 → 字幕状态不更新；
- OSC 若依赖 UI 帧，聊天框也会停在旧文本，违背"流水线启动时持续更新"。

因此推荐：**OSC 作为独立后台任务，直接消费会话事件，不依赖 GUI 重绘**。

#### 4.5.2 数据流

```text
pipeline::session::run
  └─ emit(PipelineEvent)
       ├─ GUI 事件通道 ──▶ KaolsApp::pump_events → SubtitleState → 界面
       └─ OSC 事件通道 ──▶ ChatboxService 任务
                              ├─ 自己的 SubtitleState（与 GUI 同一 reducer）
                              ├─ 事件驱动 + 200ms tick
                              ├─ 文本 1 秒节流 + typing 独立
                              ├─ compose_text()（纯函数）
                              └─ rosc 编码 → UDP 127.0.0.1:port

KaolsApp
  ├─ watch::Sender<ChatboxSettings>  ──▶ OSC 任务（enabled/port/display/translate_mode）
  └─ 命令通道（发送测试消息）
```

#### 4.5.3 关键实现点

1. **`SubtitleState` 抽到独立模块**（建议 `src/subtitles.rs`）：
   - 现在在 [`src/app.rs:55`](../../src/app.rs#L55)，包含 `SubtitleLine`、`MAX_SUBTITLES`、reducer 与测试；
   - GUI 与 OSC 服务必须用**同一份**归约逻辑，否则翻译模式的事件语义（D26）会漂移；
   - 这是纯移动重构，现有单测跟着搬，风险低；同时让 `app.rs` 变小。
   - **不需要**新增 `last_finalized_at` 之类的 5 秒计时字段（v4 已取消 5 秒逻辑）。
2. **会话事件双发**：
   - `pipeline::session::run` 增加参数 `chatbox_events: Option<UnboundedSender<PipelineEvent>>`；
   - `emit` 里 `event.clone()` 先发 GUI，再发 OSC（两个独立消费者）；
   - `app.start_session` 把 `ChatboxService::event_sender()` 传进去；
   - 启动失败包装任务发 `Error + StateChanged(Failed)` 时，也向 OSC 通道发 `Failed`，
     否则中途失败后聊天框会停在最后一句。
3. **OSC 服务生命周期**：
   - 在 `KaolsApp::new` 里用 `runtime.spawn` 启动一次，整个应用生命周期常驻；
   - 即使 `[vrchat.osc] enabled=false` 也保持空转并消费事件（这样运行中打开开关能立即显示当前句）；
   - 应用启动/重新启用 OSC 时先发一次 `typing=false`，清掉上次异常退出可能残留的 typing 指示；
   - 关闭开关/停止会话/失败：发 `""` + `typing=false` 清空后空闲；
   - 退出应用：会话停止已触发清空；若空闲时仍有残留文本，可在关窗流程补一次清空。
4. **配置实时生效**：
   - `KaolsApp` 用 `tokio::sync::watch` 把 `ChatboxSettings { osc, translate_enabled }` 发给 OSC 任务；
     `reload_config` / 写回成功后更新；
   - 不用把 `[vrchat.*]` 放进 `config_signature`，因此不会触发 D20 重启；
   - 端口变了：向旧端口尽力清空 + typing=false → 新建 socket → 用新端口。
5. **发送层**：
   - `ChatboxSender` 持有 `UdpSocket`（`bind("127.0.0.1:0")`、`set_nonblocking(true)`）、目标端口；
   - `send_text` / `send_typing` / `send_clear`：`rosc::encoder::encode` → `send_to`；
     `WouldBlock` 直接丢弃并记录一次；
   - 不要 bind 9000；不要用 bundle；不要依赖 `send_to` 的错误判断 VRChat 是否运行。
6. **日志与错误**：
   - 发送失败只 `log::debug!`/`log::warn!`（限频），不弹全局错误；
   - 不做界面状态行；配置端口非法（0）属于配置错误，走正常配置校验/页内红字；
   - VRChat 未运行是正常状态，不是用户可感知的失败。

#### 4.5.4 备选（更小改动，不推荐作为最终形态）

如果第一阶段想快速验证，也可以先在 `KaolsApp::ui()` 末尾调用 `chatbox.tick(...)`（用
`ctx.request_repaint_after(200ms)` 驱动 pending 清空/typing），复用同一套 compose/发送代码。
它的风险是窗口最小化/遮挡时停更；验证通过后再迁移到 4.5.2 的后台服务。**最终形态建议后台服务。**

### 4.6 文件改动清单

| 文件 | 改动 | 测试 |
|---|---|---|
| `Cargo.toml` | 增加 `rosc = "0.11"`（默认 features） | `cargo build --bins` |
| `src/config.rs` | `VrchatConfig` + `VrchatOscConfig` + `VrchatDisplay` + 默认值 + `port != 0` 校验 + 解析测试 | 默认值、嵌套段、未知字段、port=0、display 解析 |
| `src/config_edit.rs` | `set_vrchat_osc` + 嵌套表创建 + 保留其它 `[vrchat.*]`/注释 | 写回、创建缺失表、`display` 序列化 |
| `src/subtitles.rs`（新） | 从 `app.rs` 抽出 `SubtitleState`/`SubtitleLine`/`MAX_SUBTITLES` | 搬现有 reducer 测试 |
| `src/chatbox.rs`（新） | `compose_text()` / `truncate_chatbox()` / typing 状态判断纯函数 | 四种模式、30s 自然过期语义、144/9 行、双语优先级、空译文、typing 跳变 |
| `src/osc.rs`（新） | `ChatboxService` 任务、`ChatboxSender`、1 秒文本节流、typing、测试消息 | 编码（用 `rosc::decoder` 反解）、1 秒节流（注入时钟）、清空、端口切换、typing 独立发送 |
| `src/events.rs` | 不变（不新增事件变体） | — |
| `src/pipeline/session.rs` | `run` 增加 `chatbox_events`，`emit` 双发 | 现有会话测试/编译 |
| `src/app.rs` | `Tab::Vrchat`、`ALL` 7、`central` 分支；持有 `ChatboxService`；`start_session` 传 sender；失败时双发 `Failed`；`sync_from_config` 更新 watch；`update_vrchat_settings`；`send_test_message` | 现有 app 测试 + 配置指纹测试（断言 `[vrchat.*]` 不影响指纹） |
| `src/gui/mod.rs` | `pub mod vrchat_tab;` | — |
| `src/gui/vrchat_tab.rs`（新） | 页面 UI（≤300 行） | egui_mcp 手工 |
| `src/lib.rs` | `pub mod subtitles; pub mod chatbox; pub mod osc;` | 编译 |
| `docs/decisions.md` | 新增 `D33`；更新 `D3` 白名单、`D20` 指纹例外、`D6` 第二消费者说明 | 文档审查 |
| `docs/architecture.md` | 模块树、配置表、GUI 表、配置指纹说明、新增第 17 节、测试数 | 文档审查 |
| `README.md` | 页签表新增 VRChat 行；新增「VRChat 聊天框（OSC）」章节、配置示例、故障排查 | 文档审查 |
| `config.example.toml` | 新增 `[vrchat.osc]` 注释段 | `config` 模板测试 |
| `CONTRIBUTING.md` | §6 配置白名单加 `[vrchat.osc]`；§3 测试落点加"OSC 编码/UDP"一行 | 文档审查 |

### 4.7 需要新增的设计决定（D33 草案）

```text
D33 VRChat OSC 聊天框输出

决定：
- 配置放 [vrchat.osc]（父段 [vrchat] 预留给未来功能）：enabled / port / display。
  默认 false / 9000 / bilingual；GUI 可写这三个字段（D3 白名单扩展）。
- 发送到 127.0.0.1:port 的 /chatbox/input，参数 (text, true, false)；
  一条一个 UDP 包，不用 bundle；typing 状态跳变时发 /chatbox/typing bool。
- 文本最小发送间隔固定 1000ms（实测：900ms 长跑跳字，1000ms + 144 字符 + 每秒 typing 稳定）；
  1 秒内多次变化只发最新内容；相同文本只在同一段显示内去重，新当前句/打字输入会重置去重。
- typing 不占文本的 1 条/秒预算（实测 1000ms 文本 + 每秒 typing 连续 120 秒正常）；
  只在当前句开始/结束跳变时发。
- 文本上限 144 字符、9 行；客户端截断；截断时**保留句尾、前导 `…`**；双语按"译文\n原文"。
- 聊天框在最后一个文本包后约 30 秒自然消失（实测）；句终后**保留最后文本、不主动清空**；
  停止/失败/关闭 OSC/关窗才显式发空字符串（实测立即清空）；typing 只在当前句活跃时为 true。
- 三种模式只对翻译模式生效；非翻译模式固定显示 ASR 原文。
- 独立后台 OSC 任务消费会话事件的克隆；不依赖 GUI 重绘；整个 [vrchat.*] 不进入 D20
  配置指纹，改设置不重启实时管线。
- 发送失败只记日志，不弹错误横幅（VRChat 未运行是正常状态）。

理由：
- 聊天框是"旁路文本输出"，不应把它的开关/端口/显示模式耦合到 ASR/TTS 生命周期；
- 1 秒是本机实测的稳定下限（社区 1500ms 偏保守），同时保留 144 字符与 typing 压力下的稳定；
- typing 实测不挤占文本预算，但只在跳变时发，减少无意义消息；
- 独立任务保证窗口最小化/VR 全屏时仍然持续更新；
- 复用 SubtitleState 的归约语义，避免 D16/D26 的字幕行为在聊天框侧漂移。
```

---

## 5. 风险、边界与待确认项

### 5.1 风险表

| 风险 | 影响 | 应对 |
|---|---|---|
| 1 秒间隔在长时间/其它机器上仍可能跳字 | 字幕偶尔丢一条 | 正式实现后做 10 分钟浸泡；如果出现，调到 1050–1100ms；可加一个隐藏常量（不暴露 UI） |
| 文本更新只有 1 条/秒 | 当前句字幕 1 秒一步 | 这是 VRChat 端实测上限；GUI 字幕仍逐事件更新，聊天框只做低频镜像 |
| VRChat 版本变化改变限流 | 1 秒不安全 | 真机回归；必要时调整常量 |
| typing 未来也占限流 | 文本更新被挤掉 | 当前实测不占；若版本变化，把 typing 纳入 1 秒预算/加开关 |
| 144 计数单位不确定 | 中文/emoji 可能被截断 | 按 UTF-16 码元 144 预算；真机测 CJK；必要时降到 140 或按字节 |
| 9 行上限 | 多行打字输入被裁 | 合成时截到 9 行 |
| 自然过期时间可能变化 | 30 秒不是精确契约 | 只作为兜底；停止/关闭仍显式清空 |
| 当前句超过 ~30 秒没有任何文本变化 | 聊天框自然过期，当前句暂时不可见 | 极罕见（ASR 通常持续修订/句终）；P2 观察，必要时加 10–20 秒保活重发 |
| 窗口最小化 | UI 帧停摆导致停更 | 独立后台 OSC 任务 |
| VRChat 未运行 | 无显示 | UDP 无连接；不报错；仅写日志 |
| 端口不匹配/被占用 | 无显示 | 端口可配；`netstat -ano \| findstr :9000` 排查；文档写明 `--osc` 三段式 |
| 隐私 | 附近玩家看到说话内容/翻译 | 默认关闭；README 说明；建议私人世界测试 |

### 5.2 边界

- 只支持 VRChat 与 kaols 在同一台 Windows 机器（loopback）；不支持远程 VRChat。
- 只发不收：不实现 `/avatar/*`、OSCQuery；`/chatbox/typing` **发**（v1 使用）。
- 不持久化聊天框历史；句终后保留最后文本，由 VRChat 约 30 秒自然过期；重启后不会恢复旧会话文本。
- 多开不同目录的 kaols 会同时往 9000 发，聊天框会互相覆盖；单实例只能防同目录，文档提示即可。
- 聊天框文本是最终用户可见内容，翻译错误会直接展示；v1 不做"译文/原文"标签。
- 文本 1 秒间隔是实测结论，不是官方保证；升级 VRChat 或换机器需要复测。

### 5.3 为什么 v1 不做 OSCQuery

VRChat 2023.3.1+ 支持 OSCQuery，可以自动发现 VRChat 的 OSC 端口，避免手动配置。
但它需要额外的 mDNS 依赖（如 `mdns-sd`）+ HTTP 查询 + 端口解析；而本功能默认端口 9000 稳定可用，
用户手动改端口是低频操作。v1 用"手动端口"最简单、最少依赖；以后可作为增强项
（`[vrchat.osc] port` 仍保留，OSCQuery 作为可选自动发现）。

---

## 6. 测试与验收计划

### 6.1 单元测试（不联网）

| 模块 | 用例 |
|---|---|
| `config` | 空 `[vrchat.osc]` 取默认；`port=0` 报错；`display` 三种值解析；未知字段报错；模板可解析 |
| `config_edit` | `set_vrchat_osc` 创建缺失的 `[vrchat.osc]`、保留其它 `[vrchat.*]`/注释、无改动不写盘、`.bak` 行为 |
| `chatbox` compose | 非翻译模式忽略 display；source/bilingual/translation；当前句优先；句终后最后文本保留（不清空）；译文为空时 Keep；双语缺一；打字句 |
| `chatbox` truncate | 144 码元截断、**保留句尾/前导 `…`**、9 行保尾、双语预算、`\r\n` 归一、中文/emoji 边界不 panic |
| `chatbox` typing | 当前句 true；历史/清空 false；只在跳变时发；translation-only 无译文但当前句存在时仍 true |
| `osc` encode | 用 `rosc::decoder` 反解：`/chatbox/input` 的 `String` + `Bool(true)` + `Bool(false)`；`/chatbox/typing` 的 `Bool` |
| `osc` 节流 | 注入假时钟：1 秒内连续变化只发一次、到点发最新；相同文本去重；**新当前句/打字输入重置去重**；typing 不更新文本窗口；清空；端口切换 |
| `subtitles` | 搬现有测试；不再需要 5 秒计时字段 |

### 6.2 本地 UDP 模拟（不需要 VRChat）

用 `python-osc`（官方 OSC 文档点名的库）在本机 9100 监听，kaols 端口设 9100：

```python
from pythonosc import dispatcher, osc_server

def on_any(addr, *args):
    print(addr, args)

d = dispatcher.Dispatcher()
d.set_default_handler(on_any)
osc_server.BlockingOSCUDPServer(("127.0.0.1", 9100), d).serve_forever()
```

```bash
pip install python-osc
python osc_dump.py
```

能验证：地址、参数类型/顺序、文本内容、1 秒文本间隔、typing true/false 独立发送、
空串清空、端口切换。VRChat 在运行时 9000 被占用，脚本要换 9100，kaols 也指向 9100。

官方推荐的可视化抓包工具是 **Protokol**（<https://hexler.net/protokol>），可看原始包和参数类型；
VRChat 内置的 **OSC Debug**（Action Menu → OSC → OSC Debug）可确认 VRChat 是否收到。

### 6.3 GUI 验证（egui_mcp）

`EGUI_INSPECTION=1` + `scripts/dev-run.ps1`，用 egui_mcp：

- 打开「VRChat」页，截图核对页签位置、默认关闭、端口/模式控件、"发送测试消息"；
- 点击开关/模式/改端口，确认写回 `[vrchat.osc]`、不触发重启（状态灯不变、当前句不丢）；
- 点击「发送测试消息」，同时看本地 UDP 监听和 VRChat 聊天框（应**立即**出现，不等 1 秒）；
- 确认页面没有状态行/typing 说明/隐私提示；
- 最小化窗口后再恢复，确认后台 OSC 仍在更新（这是 4.5 架构的关键验证）。

### 6.4 真机验收矩阵（VRChat，约 10–15 分钟）

| # | 场景 | 预期 |
|---|---|---|
| 1 | OSC 关闭 | 本地监听收不到任何包；聊天框不变 |
| 2 | OSC 开，非翻译模式，说话 | 聊天框最多 1 秒更新一次，显示 ASR 最新文本；句终后保留最后文本，约 30 秒自然消失 |
| 3 | 翻译模式 + 三种 display | 内容/顺序正确；非翻译模式忽略 display |
| 4 | 翻译模式，"只显示译文" | 不出现原文；译文未到时保留上一帧 |
| 5 | 打字输入（普通/翻译） | 作为最后文本保留，约 30 秒自然消失 |
| 6 | typing 指示器 | 当前句时亮；句终/清空时灭；不随每次文本更新闪断 |
| 7 | 停止会话 / 关闭 OSC / 改端口 | 聊天框清空；typing 灭；旧端口不再收包 |
| 8 | 中文、日文、emoji、多行、144 字符 | 显示正常；144/9 行截断符合预期 |
| 9 | 连续运行 10 分钟 | 无跳字、无 `Timed out for spam`；最长间隔接近 1 秒 |
| 10 | VRChat 不运行 | kaols 正常；无错误横幅；无状态行；页面不报错 |
| 11 | 最小化 kaols 后说话 | 聊天框仍持续更新（后台任务） |

### 6.5 实测记录（2026-11-08，已完成）

临时工具已删除，结论保留：

- 900ms 20 秒：无跳字；850ms 20 秒：约 24 条跳 1–2 条；
- **900ms 120 秒：到后来会跳字**；
- **1000ms 120 秒 + 144 字符 + 每秒 typing：始终正常，无跳字，typing 指示器每秒切换**；
- **单条消息后不再发包：约 30 秒自然消失**（20:59:15.458 发，20:59:45 消失）；
- **空字符串立即清空**（21:01:29.478 发，21:01:34.714 清空，约 34.7 秒立即消失）；
- UDP 错误 0；未观察到 `Timed out for spam`。

因此固定文本 1 秒；句终后不做 5 秒清空，保留最后文本等 VRChat 约 30 秒自然过期；
停止/关闭 OSC 显式清空。正式实现后只需做 6.4 的 10 分钟浸泡；如果未来复测，可临时重建发送工具，
或直接用 kaols 的「发送测试消息」+ 真实语音验证。

---

## 7. 建议的调试环境

### 7.1 结论

**不需要全程打开 VRChat，但最终验收需要真机。** 建议分三段：

1. **纯逻辑/协议开发（不打开 VRChat）**：单元测试 + 本地 UDP 模拟接收端 + `rosc::decoder`。
   这能覆盖大部分 bug（编码、参数、1 秒节流、typing、自然过期/清空、截断、端口）。
2. **UI/联调（不打开 VRChat）**：egui_mcp 操作新页面，配合 9100 的模拟接收端；
   检查开关/模式/端口/测试按钮，确认写回与不重启。
3. **真机验收（打开 VRChat）**：
   - 中文/日文/emoji 的真实渲染与 144/9 行表现；
   - 空串清空、typing 指示器（两者行为已实测，验收实现调用）；
   - 1 秒文本间隔下 10 分钟无跳字/无 spam timeout；
   - 句终后约 30 秒自然消失，停止/关闭时立即清空；
   - `Show Own ChatBox` 下你能看到自己的聊天框。

### 7.2 VRChat 端设置

1. 启动 VRChat（桌面模式即可；本机日志 `XR Device: None`，没有 VR 也完全能测）。
2. 进入一个**私人世界 / 自己的 home world**，不要在公共场景测试。
3. 确认 OSC：Action Menu → Options → OSC → Enabled；
   可选打开 **OSC Debug** 看 VRChat 是否收到包（它也会顺带开启 OSC）。
4. 打开 VRChat 设置里的 **Show Own ChatBox**（否则自己的聊天框通常不显示）。
5. 如果改了端口：VRChat 启动参数是 `--osc=inPort:senderIP:outPort`
   （默认 `--osc=9000:127.0.0.1:9001`），Steam 启动选项里设置；kaols 的「发送端口」填相同的 `inPort`。
   不要用 `--osc=9000` 单值写法（官方未文档化）。
6. 建议 VRChat 窗口保持可见（不要最小化）：AI 可用 Windows 截图辅助；
   VR 模式/AI 无法可靠读小字，最终仍要你确认。

### 7.3 kaols / AI 端设置

```powershell
# 1) 开发运行（自动把仓库 config.toml 复制到 target\debug）
$env:EGUI_INSPECTION=1
.\scripts\dev-run.ps1

# 2) 测试配置（或直接在「VRChat」页改）
# [vrchat.osc]
# enabled = true
# port = 9000
# display = "bilingual"

# 3) 本地模拟接收（VRChat 不开时；端口 9100 避免和 VRChat 9000 冲突）
pip install python-osc
python osc_dump.py   # 监听 127.0.0.1:9100
```

- AI 能自动做：改代码、跑 `cargo test`、egui_mcp 点击/截图、抓 UDP 包、读 kaols 日志、
  读 VRChat 日志、在桌面模式截 VRChat 窗口。
- AI 不能可靠做：VRChat 登录/2FA、Steam 启动、VR 内观察、判断聊天框小字是否清晰。
  这些交给人工，一次 5–10 分钟即可。
- 建议在「VRChat」页保留 **发送测试消息** 按钮（不做状态行）：人工验收时不用说话/开 API，
  点一下就能确认链路；AI 也能通过它做自动化冒烟。
- 建议加日志：每次实际发送 `debug!` 记录端口/字符数/kind/是否等待 1 秒窗口；
  这样 AI 在没有 VRChat 时也能从 `logs\kaols-YYYY-MM-DD.log` 判断发送行为。

### 7.4 推荐的协作节奏

1. 你先在私人世界打开 VRChat 并保持运行（桌面模式，Show Own ChatBox 打开）；
2. AI 完成 P0 代码 + 单测 + 9100 模拟端验证；
3. AI 用 egui_mcp 在真实 UI 里点「发送测试消息」；你看聊天框是否出现、是否无声、typing 是否为 false；
4. 你用真实语音跑 10 分钟，检查 1 秒更新、句终后自然过期、typing 当前/历史、无跳字；
5. 收尾：更新文档、`cargo fmt/build/test`、人工冒烟。

---

## 8. 已确认与待验收

### 8.1 已确认

1. **配置嵌套**：`[vrchat.osc]`，父段 `[vrchat]` 给以后的 VRChat 功能预留。
2. **文本限流**：固定 **1 秒**（900ms 长跑跳字；1000ms + 144 字符 + 每秒 typing 稳定）。
3. **typing 不占用文本限流**：每秒 typing + 1000ms 文本连续 120 秒正常。
4. **发送测试消息**：VRChat 页提供。
5. **双语顺序**：`译文\n原文`。
6. **"只显示译文"但译文未到**：保留上一帧、不临时显示原文。
7. **停止/关闭 OSC**：显式清空（已实测立即生效）；正常句终保留最后文本，由 VRChat 约 30 秒自然过期。
8. **typing**：当前句活跃时 true，历史/清空时 false；只在跳变时发。

### 8.2 正式验收项

1. 1 秒文本间隔在 10 分钟真实语音/翻译下无跳字、无 `Timed out for spam`。
2. 中文/日文/emoji、144 字符/9 行截断。
3. typing 指示器在桌面模式/自己视野里的实际效果。
4. 句终后最后文本约 30 秒自然消失；停止/关闭 OSC 时显式清空（已实测，验收实现调用正确）。
5. 不同世界/网络条件下 30 秒自然过期的稳定性（它只是兜底，不精确依赖）。

---

## 9. 分阶段实施建议

| 阶段 | 内容 | 验收 |
|---|---|---|
| P0 核心 | `[vrchat.osc]` 配置 + `config_edit` + `subtitles` 抽取 + `chatbox` 纯逻辑 + `osc` 服务（1 秒文本节流、typing 独立、自然过期、状态）+ 会话事件双发 + GUI 页 + 单测 | `cargo fmt/build/test`；9100 模拟端；1s/typing/自然过期/截断/清空正确 |
| P1 可用性 | 发送测试消息、日志、README/示例配置/故障排查、D33 | egui_mcp 截图；人工点测试消息 |
| P2 真机验收 | 6.4 的 10 分钟浸泡、中文/emoji/长文本、typing 指示器 | 无跳字、无 spam timeout；聊天框显示正确 |
| 后续（可选） | OSCQuery 自动发现、保活重发、typing 开关、更多 `[vrchat.*]` 功能 | 另开决定/PR |

---

## 10. 参考资料

**官方**

- OSC 总览（端口、启用、工具）：<https://docs.vrchat.com/docs/osc-overview>
- OSC as Input Controller（ChatBox/typing 章节）：<https://docs.vrchat.com/docs/osc-as-input-controller#chatbox>
- OSC Debugging：<https://docs.vrchat.com/docs/osc-debugging>
- OSCQuery：<https://docs.vrchat.com/docs/oscquery>
- Launch Options：<https://docs.vrchat.com/docs/launch-options>
- 官方 OSC GitHub / wiki（旧 ChatBox 页）：<https://github.com/vrchat-community/osc/wiki/Chatbox>
- 官方 OSCQuery wiki：<https://github.com/vrchat-community/osc/wiki/OSCQuery>

**限流/实践（社区）**

- VRCOSC FAQ（1500ms 下限、调大间隔解决 spam timeout）：<https://vrcosc.com/docs/faq>
- VRCOSC 源码（默认 1500ms）：<https://github.com/VolcanicArts/VRCOSC>
- VRC-OSC ChatBox 插件（1.5s 最快）：<https://github.com/ShayBox/VRC-OSC>
- Chatbox rate limiting（官方反馈板）：<https://vrchat.canny.io/bug-reports/p/chatbox-rate-limiting-too-aggressive>
- Timed out for spam 误报讨论：<https://vrchat.canny.io/bug-reports/p/timed-out-for-spam-30-secs-remaining-false-positive>
- OSC Chatbox Assistant（日文/CJK 发送实践）：<https://soramimi.github.io/OSC-ChatboxAssistant/>
- 日文 ChatBox 实测（2024-12）：<https://zenn.dev/kx_ras/articles/681cf6df73430b>

**Rust**

- `rosc`：<https://crates.io/crates/rosc>、<https://docs.rs/rosc/0.11.4/rosc/>
- `osc`（不建议）：<https://crates.io/crates/osc>
- `nannou_osc`：<https://crates.io/crates/nannou_osc>
- `vrchat_osc`：<https://crates.io/crates/vrchat_osc>
- `vrchat-box`（CLI 参考实现）：<https://crates.io/crates/vrchat-box>

**本仓库**

- 架构：<../architecture.md>
- 设计决定：<../decisions.md>
- 配置模板：<../../config.example.toml>
- 用户说明：<../../README.md>
- 贡献/门禁：<../../CONTRIBUTING.md>
