# Silero VAD 模型（随仓库分发）

`silero_vad_16k_op15.onnx` 是 Silero VAD 官方提供的 **16kHz 单采样率 ONNX 导出**（opset 15），
供 kaols 的本地语音活动检测（VAD）使用；P1 起通过 `include_bytes!` 嵌入 exe，不随发布包单独分发。

## 来源与版本

- 上游仓库：<https://github.com/snakers4/silero-vad>
- 上游版本：v6.2.3（2026-09-23 发布；本文件取自 2026-11-12 的 `master` 分支 `src/silero_vad/data/`）
- 原始路径：`src/silero_vad/data/silero_vad_16k_op15.onnx`
- 大小：1,289,603 字节
- SHA256：`7ed98ddbad84ccac4cd0aeb3099049280713df825c610a8ed34543318f1b2c49`
- 许可：MIT（见同目录 [`LICENSE`](LICENSE)，Copyright (c) 2020-present Silero Team）

## 接口契约（P0 实测确认）

- 采样率固定 **16000 Hz**；每帧恰好 **512 个采样（32ms）**。
- 输入：
  - `input` f32 `[1, 576]` = 64 采样 context + 512 采样新帧；
  - `state` f32 `[2, 1, 128]`（初值全零）；
  - `sr` i64 标量 = `16000`。
- 输出：
  - `output` f32 `[1, 1]`：该帧为语音的概率；
  - `stateN` f32 `[2, 1, 128]`：下一帧要回传的 state。

## 选型依据

16k_op15（1.23MiB）与通用 `silero_vad.onnx`（8k/16k，2.22MiB）在同一段 60s 语音上
**逐帧概率完全一致**（静音 p50≈0.002、白噪声最大 0.023、语音 77.4% 帧 ≥0.5），
且单帧推理均约 0.13ms；前者小 1MiB、会话加载更快，因此选用它。

不要手工修改该文件。升级模型时替换文件，并同步更新本 README 的版本、大小与 SHA256。
