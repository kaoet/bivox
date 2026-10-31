# kaols (超低延迟流式语音识别与语音合成客户端)

面向实时对话与 VRChat 场景的 **超低延迟** 语音识别与合成客户端原型（MVP）。

不同于传统工具（如 VRCLS）的“整句录音等待 $\to$ 批量上传 $\to$ 整句 TTS”瀑布式流程，`kaols` 采用 **全双工流式流水线**：
1. **音频采集**：通过 `cpal` 原生采集麦克风输入，100ms 切片持续推流。
2. **实时流式识别**：直连阿里百炼 **`qwen-audio-3.1-asr-flash-streaming`**，在服务端流式输出转写文本。
3. **流式切片 (Streaming Chunking)**：命中标点或达到 5~8 词语义断点时，立即切块触发语音合成，无需等待整句说完。
4. **CosyVoice 实时合成**：通过百炼 **`cosyvoice-v3.5-flash`** WebSocket 流式接收 PCM 音频。
5. **无缝平滑播放**：环形缓冲队列平滑连续播放，并支持新发言或打断信号即时清空旧语音。

---

## 快速上手

### 1. 配置百炼 API Key

在项目根目录下创建 `.env` 文件（或参考 `.env.example`）：

```env
DASHSCOPE_API_KEY=sk-your-dashscope-api-key-here
```

也可以通过环境变量或启动参数指定：

```bash
cargo run -- --api-key "sk-..."
```

---

### 2. 百炼业务空间专属域名（已配置默认值）

百炼已为华北2（北京）/ 新加坡推出业务空间专属域名（`{WorkspaceId}.cn-beijing.maas.aliyuncs.com`），官方建议替换旧的 `dashscope.aliyuncs.com`。本项目 ASR / CosyVoice 两条 WebSocket 链路均已使用该域名：

```bash
# 默认：ws-e01bu1mp0vk0o909.cn-beijing.maas.aliyuncs.com
cargo run --release

# 切换到其它业务空间 / 地域
cargo run --release -- --workspace-host ws-xxxxxxxx.cn-beijing.maas.aliyuncs.com
# 或设置环境变量 DASHSCOPE_WORKSPACE_HOST
```

> ASR 固定使用近场 VAD 模型（`vad_model=near_meeting_16k`），适配"识别自己麦克风近讲"的使用场景。

---

### 3. 常用运行命令

#### 默认启动（使用系统默认麦克风与扬声器，预置音色）：
```bash
cargo run --release
```

#### 查看系统中所有可用音频设备（输入与输出）：
```bash
cargo run -- --list-devices
```

#### 指定特定麦克风与虚拟声卡（输出到 VRChat）：
```bash
# 例如指定 VB-Cable 虚拟音频线作为输出：
cargo run --release -- --input-device "Microphone" --output-device "CABLE Input"
```

#### 指定识别模型或合成音色：
```bash
cargo run --release -- --asr-model qwen-audio-3.1-asr-flash-streaming --voice longxiaochun
```

#### 本地 VAD（默认开启，节省 ASR 费用）：

默认启用本地 VAD：只上传语音段，静音不上传；
静音期每 10 秒补 100ms 静音保活，句末自动补 ~1s 静音以触发服务端断句。

```bash
# 默认（VAD 开启）
cargo run --release

# 关闭 VAD（全量上传，便于对比 / 排查）
cargo run --release -- --no-vad

# 打印每个 100ms chunk 的电平、噪声底、阈值与发送决策，用于现场标定
cargo run --release -- --vad-debug

# 可调参数：起音阈值 = 噪声底 + margin dB；hangover = 语音结束拖尾；tail = 句末静音
cargo run --release -- --vad-margin-db 9 --vad-hangover-ms 500 --vad-tail-ms 1000
```

调参提示：安静说话被切掉时降低 `--vad-margin-db`（如 6）；键盘/环境噪声频繁触发时提高它（如 12）。
