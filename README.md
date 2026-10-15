# kaols (超低延迟流式同传与语音合成客户端)

面向实时对话与 VRChat 场景的 **超低延迟** 语音同传与合成客户端原型（MVP）。

不同于传统工具（如 VRCLS）的“整句录音等待 $\to$ 批量上传 $\to$ 整句 TTS”瀑布式流程，`kaols` 采用 **全双工流式流水线**：
1. **音频采集**：通过 `cpal` 原生采集麦克风输入，100ms 切片持续推流。
2. **实时流式同传**：直连阿里百炼 **`qwen3.8-livetranslate-flash-realtime`** Realtime WebSocket，在服务端直接流式输出识别与同传译文。
3. **投机切片 (Speculative Chunking)**：命中标点或达到 5~8 词语义断点时，立即切块触发语音合成，无需等待整句说完。
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

### 2. 常用运行命令

#### 默认启动（使用系统默认麦克风与扬声器，中文 -> 英文，预置音色）：
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

#### 切换语言方向（例如中译日）：
```bash
cargo run --release -- --source-lang zh --target-lang ja
```

#### 启用声音克隆模式（需提供 3~10 秒参考音频与对应文本）：
```bash
cargo run --release -- --ref-audio "./my_voice.wav" --ref-text "这是参考音频所说的文本内容"
```

#### 关闭翻译功能（直接语音转文字后转语音，纯 ASR -> TTS）：
```bash
# 默认使用百炼 qwen-audio-3.1-asr-flash-streaming 双向流式大模型进行极速转写
cargo run --release -- --no-translate

# 也可以显式指定 ASR 模型或音色：
cargo run --release -- --no-translate --asr-model qwen-audio-3.1-asr-flash-streaming --voice longxiaochun
```

