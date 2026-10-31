# kaols (超低延迟流式语音识别与语音合成客户端)

面向实时对话与 VRChat 场景的 **超低延迟** 语音识别与合成客户端原型（MVP）。

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

`config.toml` 已被 `.gitignore` 忽略，不会被提交。所有配置项都有默认值，未写出的字段使用内置默认值，完整字段见 [`config.example.toml`](config.example.toml)：

```toml
[api]
key = "sk-..."
# workspace_host = "dashscope.aliyuncs.com"

[asr]
# model = "qwen-audio-3.1-asr-flash-streaming"

[tts]
# model = "qwen-audio-3.1-tts-flash"
# voice = "qiaoxiaojiao_v3.1"

[audio]
# input_device = "Microphone"
# output_device = "CABLE Input"

[vad]
# enabled = true
# margin_db = 9.0
# hangover_ms = 500
# tail_ms = 1000
# debug = false

[chunking]
# words = 6
```

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

如果控制台开通了业务空间专属域名（`{WorkspaceId}.cn-beijing.maas.aliyuncs.com`，百炼建议华北2 / 新加坡等地域使用），可在 `config.toml` 中覆盖：

```toml
[api]
workspace_host = "ws-xxxxxxxx.cn-beijing.maas.aliyuncs.com"
```

`workspace_host` 只填写纯 host，不要带 `wss://` 前缀、路径或端口；格式不规范时程序会在启动阶段直接报错。

> ASR 固定使用近场 VAD 模型（`vad_model=near_meeting_16k`），适配"识别自己麦克风近讲"的使用场景。

---

### 3. 常用运行命令

#### 默认启动（使用系统默认麦克风与扬声器，预置音色）：
```bash
cargo run --release
```

#### 查看系统中所有可用音频设备（输入与输出，无需配置文件）：
```bash
cargo run -- --list-devices
```

#### 指定特定麦克风与虚拟声卡（输出到 VRChat）：
```toml
[audio]
input_device = "Microphone"
output_device = "CABLE Input"
```

设备名称支持模糊匹配（不区分大小写）；留空或不写表示系统默认设备。

#### 指定识别模型或合成音色：
```toml
[asr]
model = "qwen-audio-3.1-asr-flash-streaming"

[tts]
model = "qwen-audio-3.1-tts-flash"
voice = "qiaoxiaojiao_v3.1"   # 系统音色；也可填同模型下复刻/设计音色的 voice_id
```

#### 本地 VAD（默认开启，节省 ASR 费用）：

默认启用本地 VAD：只上传语音段，静音不上传；
静音期每 10 秒补 100ms 静音保活，句末自动补 ~1s 静音以触发服务端断句。

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

## 命令行参数

业务配置全部来自 `config.toml`，命令行只保留两个入口：

| 参数 | 说明 |
|---|---|
| `-c, --config <path>` | 指定 TOML 配置文件路径（默认 `./config.toml`） |
| `--list-devices` | 列出所有可用音频输入/输出设备后退出（不读取配置文件） |
| `-h, --help` | 查看帮助 |
