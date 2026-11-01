# kaols (超低延迟流式语音识别与语音合成客户端)

面向实时对话与 VRChat 场景的 **超低延迟** 语音识别与合成客户端原型（MVP）。

> **迁移中**：本仓库正在从 CLI 迁移到纯 GUI（计划与决策见 [GUI_MIGRATION.md](GUI_MIGRATION.md)）。
> 当前 `master` 已有 GUI 骨架（M0）与独立的百炼音色调试 CLI `admin`（M4）；
> 下面的 CLI 用法在迁移完成前仍然有效，迁移完成后会被 GUI 用法替换。

不同于传统工具（如 VRCLS）的“整句录音等待 $\to$ 批量上传 $\to$ 整句 TTS”瀑布式流程，`kaols` 采用 **全双工流式流水线**：
1. **音频采集**：通过 `cpal` 原生采集麦克风输入，100ms 切片持续推流。
2. **实时流式识别**：直连阿里百炼 **`qwen-audio-3.1-asr-flash-streaming`**，在服务端流式输出转写文本。
3. **流式切片 (Streaming Chunking)**：命中标点或达到 5~8 词语义断点时，立即切块触发语音合成，无需等待整句说完。
4. **Qwen-Audio-TTS 实时合成**：通过百炼 **`qwen-audio-3.1-tts-flash`** WebSocket 流式接收 PCM 音频。
5. **无缝平滑播放**：环形缓冲队列平滑连续播放，并支持新发言或打断信号即时清空旧语音。

---

## 快速上手

### 1. 创建配置文件

复制示例配置，并填入百炼 API Key（获取地址：https://bailian.console.aliyun.com/）：

```bash
cp config.example.toml config.toml     # Windows: copy config.example.toml config.toml
```

```toml
[api]
key = "sk-your-dashscope-api-key-here"
```

`config.toml` 已被 `.gitignore` 忽略，不会被提交。所有配置项都有默认值，完整字段见 [`config.example.toml`](config.example.toml)。

默认读取当前工作目录下的 `config.toml`；也可以指定其它路径：

```bash
cargo run --release -- --config path/to/config.toml
# 或使用短参数
cargo run --release -- -c path/to/config.toml
```

---

### 2. 百炼接入域名（默认使用通用域名）

默认使用百炼通用接入域名 `dashscope.aliyuncs.com`，无需额外配置：

```bash
cargo run --release
```

如果控制台开通了业务空间专属域名（`{WorkspaceId}.cn-beijing.maas.aliyuncs.com`），可在 `config.toml` 中覆盖：

```toml
[api]
workspace_host = "ws-xxxxxxxx.cn-beijing.maas.aliyuncs.com"
```

`workspace_host` 只填写纯 host，不要带 `wss://` 前缀、路径或端口；格式不规范时程序会在启动阶段直接报错。语音识别/合成的 WebSocket 连接与音色管理的 HTTP 接口都使用这个域名。

---

### 3. 常用运行命令

#### 默认启动（使用系统默认麦克风与扬声器）：
```bash
cargo run --release
# 等价于显式指定默认子命令
cargo run --release -- run
```

#### 查看系统中所有可用音频设备（输入与输出，无需配置文件）：
```bash
cargo run -- devices
```

#### 指定特定麦克风与虚拟声卡（输出到 VRChat）：
```toml
[audio]
input_device = "Microphone"
output_device = "CABLE Input"
```

设备名称支持模糊匹配（不区分大小写）；留空或不写表示系统默认设备。

#### 本地 VAD（默认开启，节省 ASR 费用）：

```toml
[vad]
# 关闭 VAD（全量上传，便于对比 / 排查）
enabled = false

# 打印每个 100ms chunk 的电平、噪声底、阈值与发送决策，用于现场标定
debug = true

# 起音阈值 = 噪声底 + margin dB；hangover = 语音结束拖尾；tail = 句末静音
margin_db = 9.0
hangover_ms = 500
tail_ms = 1000
```

调参提示：安静说话被切掉时降低 `margin_db`（如 6）；键盘/环境噪声频繁触发时提高它（如 12）。

---

## 多语言识别与声线

### 1. `[asr] languages`：用户可能说的语言

```toml
[asr]
languages = ["zh", "ja", "en"]
```

- 有序列表，**首个元素是默认语言**；
- 只写一个元素（如 `["ja"]`）相当于强提示只按该语言识别；

### 2. 语言判定规则（不等待整句、不回溯）

每个 TTS task（约等于一个 ASR 句子）在首个 chunk 送出前判定一次，之后整句沿用：

- 出现**假名** → 日语；出现**谚文** → 韩语（这两类字符不会被中文/英文使用，属于强证据）；
- **纯汉字句**：当前是日语且汉字数 ≥ 6 且无假名 → 切回中文；更短的汉字句保持当前语言（粘性）；
- **拉丁字母**必须"占多数"（`拉丁数 ≥ 2` 且多于汉字）才算证据：`我找不到我的 U盘了` 不会被误判为英语；
- 文本太短（< 2 个文字）或证据不足 → 沿用当前语言；
- 判定结果不在 `languages` 列表中 → 落到 `languages[0]`；
- 不等待整句、不额外调用语言识别模型；因此句尾才出现决定性字符时可能错判一句，下一句自动纠正。

自动判定的脚本覆盖范围：

- 中日韩：假名 / 谚文 / 汉字（规则如上）；
- 拉丁字母语系：**无法区分具体语种**，归一到 `languages` 中出现的第一个拉丁语言（如 `["zh","en"]` 下识别为 en）；
- 其它可映射脚本：西里尔→`ru`、阿拉伯→`ar`、泰文→`th`、希腊→`el`、天城文→`hi`；
- 其它未覆盖文字 → 落到 `languages[0]`。

### 3. 合成语言参数

每个 TTS 句子的 `run-task` 都会带：

- `language_hints`: 本句判定出的语言（官方说明：合成目标语言，与复刻样本语种无关）；
- `instruction`: 有内置映射的非中英语种自动附带（ja/ko/fr/de/ru/es/pt/it/th/vi/id/ms/fil/ar，如 `请讲日语。`）；
  中英文不需要；其它 ASR-only 语种（nl/sv/no/hi/el 等）`language_hints` 使用 `other`、不附 instruction。

---

## 自定义音色（声音复刻，多语言声线）

无需在百炼控制台手动上传音频：在配置文件中声明每个语言的样本，程序会自动上传、复刻、缓存。

### 1. 配置

```toml
[tts]
model = "qwen-audio-3.1-tts-flash"
voice = "custom:edgar"
# cache_file = "voice_cache.json"

# 先定义父表，再写 samples 数组表
[tts.custom_voices.edgar]
name = "异环 - 埃德嘉"

[[tts.custom_voices.edgar.samples]]
language = "zh"
audio = "samples/edgar-zh.wav"

[[tts.custom_voices.edgar.samples]]
language = "ja"
audio = "samples/edgar-ja.wav"
```

- `samples` 是**有序数组**，`samples[0]` 是默认声线样本；
- 每个样本都会按（`[tts] model`, 音频内容）单独复刻一个 voice_id；
- 合成时：本句语言有对应样本 → 用该语言声线；没有 → 回退 `samples[0]`；
- 三种常见来源都适用：
  1. 同一个人多种语言录音（各自复刻，声线一致、母语发音更好）；
  2. 游戏角色不同语言由不同配音演员录制（各语言用各自配音声线）；
  3. 只有一种语言的网络样本（其它语言回退到该声线，合成时仍按目标语言发音，"凑合可用"）；
- 音频要求：清晰单说话人、至少约 5 秒（推荐 10~20 秒，单条建议不超过 60 秒，≤10 MB）、wav/mp3/m4a；
- 前缀：只由**配置键名**自动生成（去掉非字母数字、转小写；多语言追加语言代码，如 `edgarzh`/`edgarja`，≤10 字符）；建议键名使用 ASCII 小写字母/数字（非 ASCII 键名净化后可能为空，回退 `voice` 或只剩语言代码）。

### 2. 首次使用

启动程序（或执行 `kaols voices enroll`）时，对每个语言样本：

1. 读取样例音频并计算内容指纹（SHA256）；
2. 上传到百炼文件服务（`purpose=inference`）并换取临时下载 URL；
3. 调用音色复刻接口创建音色，轮询到部署完成；
4. 把 `模型 + 音频指纹 -> voice_id` 写入 `voice_cache.json`，清理临时文件。

多个语言样本会**并行注册**（首次约十几秒到几十秒）；之后启动只读一次音频并逐样本
`query_voice` 校验，**不会重复上传或复刻**。缓存命中后若云端音色被删除会自动重建；
校验请求失败（网络抖动）时继续信任缓存。

### 3. 缓存文件

- 默认位置：与 `config.toml` 同目录的 `voice_cache.json`（已被 `.gitignore` 忽略）；
- 按「TTS 模型 → 样例音频 SHA256」两层组织；每种语言样本因音频不同天然独立缓存；
- 切换模型时为该模型单独复刻并缓存，不影响其它模型记录。

### 4. 管理账号下的自定义音色

```bash
# 列出百炼账号下全部自定义音色（交叉显示本地缓存的音色名/语言/音频）
cargo run --release -- voices list

# 删除指定百炼原生 voice_id
cargo run --release -- voices delete --voice-id qwen-audio-3.1-tts-flash-edgarzh-xxxx

# 删除某个音色配置下的全部语言声线
cargo run --release -- voices delete --voice edgar

# 只删除某个音色的指定语言声线
cargo run --release -- voices delete --voice edgar --language ja

# 删除账号下全部自定义音色（交互确认；-y 跳过）
cargo run --release -- voices delete-all
cargo run --release -- voices delete-all -y

# 立即上传并注册配置里的自定义音色（可指定音色名 / 语言）
cargo run --release -- voices enroll
cargo run --release -- voices enroll edgar --language ja
```

- 多目标删除（`--voice` 命中多个语言）默认需要交互确认，`-y` 跳过；
- 删除后会同步清理对应的本地缓存记录；
- `voices list` 会清理“云端已删除”的缓存记录（列表不完整时跳过，避免误删）。

> ⚠️ `delete-all` 会删除该百炼账号下**全部**自定义音色（包括其它工具创建的），请确认后再执行。

---

## 命令行

不写子命令时默认进入运行模式（等价于 `kaols run`）：

```bash
kaols                       # 运行实时识别 -> 合成
kaols -c other.toml         # 指定配置文件运行
kaols run -c other.toml
kaols devices               # 列出音频设备后退出（无需配置文件）
kaols voices list           # 自定义音色管理
kaols voices delete --voice-id <id>
kaols voices delete --voice <配置名> [--language <code>] [-y]
kaols voices delete-all [-y]
kaols voices enroll [配置名] [--language <code>]
kaols --help                # 查看全部子命令
kaols voices --help
```

`-c/--config <path>` 是全局参数，任何子命令都可以用来指定配置文件（默认 `./config.toml`）。

> 语言代码一览：
> - 复刻样本 / 合成完整支持：`zh/en/ja/ko/fr/de/ru/pt/th/id/vi/es/it/ms/fil/ar`；
> - `[asr] languages` 还可用：`tl hi nl sv da fi no el pl cs hu ro bg hr sk`（以官方文档为准）；
> - ASR 的菲律宾语代码是 `tl`、复刻是 `fil`，程序内部会自动归一。
