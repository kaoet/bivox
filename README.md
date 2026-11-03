# kaols — 超低延迟实时语音识别 / 语音合成（Windows GUI）

基于阿里云百炼 **Qwen-Audio** 的实时语音识别（ASR）与语音合成（TTS）桌面客户端，面向实时对话、VRChat 等低延迟场景。

全双工流式流水线：麦克风采集 → 流式 ASR → 按标点/语义分块 → 流式 TTS → 平滑播放，无需等整句说完。

- 纯 GUI（egui/eframe），**便携模式**：配置、缓存、日志都在 exe 同目录，整个文件夹拷走即用；
- **单实例**：同一目录只允许开一个窗口，重复启动会激活已有窗口；
- 本地 VAD 门控、实时字幕（语言代码 + TTS 进度分色）、多语言自定义（复刻）音色。

---

## 快速开始

1. 解压发布包（`kaols.exe` + `README.md`）到一个**可写目录**；
2. 双击 `kaols.exe`：首次启动会在 exe 同目录生成 `config.toml` 并落在「设置」页；
3. 在「设置」页填入百炼 API Key（[控制台](https://bailian.console.aliyun.com/)），点「保存」；
4. 回到「实时」页点「开始」，对着麦克风说话即可看到字幕并听到合成语音。

> 单实例行为：第二次启动会尝试把已有窗口恢复到前台并退出，不会抢麦克风。

### 目录结构（便携模式）

```
kaols.exe
config.toml          # 首次启动生成；含 API Key，注意保密
voice_cache.json     # 自定义音色复刻缓存（模型 → 音频指纹 → voice_id）
logs\kaols-YYYY-MM-DD.log  # 按天日志，保留最近 7 天
crash.log            # 崩溃信息（如有）
```

程序只读 **exe 所在目录**的 `config.toml`，不读 `%APPDATA%`、当前工作目录或环境变量。

---

## 界面

| 页签 | 说明 |
|---|---|
| 实时 | 「开始 / 停止」、VAD 指示器、实时字幕（最新句在最上，语言代码在左）；支持单句复制 |
| 音频 | 选择输入 / 输出设备（完整设备名精确匹配），保存后配置热更新 |
| 语音合成 | 顶部选择「当前音色」；下方编辑自定义音色（显示名、键名、多语言声线样本、拖拽排序） |
| 设置 | API Key、接入域名、重新加载配置、打开程序目录、版本信息 |
| 底部日志 | 运行日志（5000 行环形缓冲），可直接排查连接/识别/合成问题 |

### 当前音色

顶部 Radio 二选一，未选中的一侧控件禁用：

- **现有音色**：输入框，填内置音色名（如 `qiaoxiaojiao_v3.1`）或任意百炼 voice_id；
- **自定义音色**：下拉选择 `[tts.custom_voices]` 里的条目（写入 `custom:<键名>`）。

改动后点页面右上角 **保存** 一次写回 `config.toml`；**放弃更改** 同时还原当前音色与下方的音色编辑草稿。
运行中保存会按「配置热更新」自动用新配置重启管线。

内置音色没有可查询的列表 API（官方只有文档表：[Qwen-Audio-TTS 音色列表](https://help.aliyun.com/zh/model-studio/qwen-audio-tts-voice-list)），因此用输入框填写 voice id。

---

## 配置说明

`config.toml` 首次启动自动生成，完整注释见文件本身。常用字段：

```toml
[api]
key = "sk-..."                 # 必填
# workspace_host = "ws-xxxx.cn-beijing.maas.aliyuncs.com"  # 业务空间专属域名（纯 host）

[asr]
# model = "qwen-audio-3.1-asr-flash-streaming"
languages = ["zh", "ja"]       # 有序；只写一个 = 强提示该语言

[tts]
# model = "qwen-audio-3.1-tts-flash"
voice = "qiaoxiaojiao_v3.1"    # 系统音色名 / 复刻 voice_id / custom:<配置名>

[audio]
# input_device = "Microphone (USB Audio)"
# output_device = "CABLE Input (VB-Audio Virtual Cable)"   # 不填 = 系统默认

[vad]
enabled = true                 # 只上传语音段，省 ASR 费用
margin_db = 9.0
hangover_ms = 500
tail_ms = 1000

[chunking]
words = 6                      # 投机分块阈值
```

`[audio]` 设备名按**完整名称精确匹配**：设备改名/被拔掉会明确报错，不会静默回退。

### 自定义（复刻）音色

一个音色可以有多份不同语言的声线样本，合成时按本句判定的语言选样本（缺该语言回退第一个样本）：

```toml
[tts.custom_voices.edgar]
name = "异环 - 埃德嘉"

[[tts.custom_voices.edgar.samples]]
language = "zh"
audio = "samples/edgar-zh.wav"

[[tts.custom_voices.edgar.samples]]
language = "ja"
audio = "samples/edgar-ja.wav"
```

- 样本音频：清晰单人、≥5 秒（推荐 10–20 秒）、≤10 MB，wav/mp3/m4a；
- 每个样本按（TTS 模型, 音频内容指纹）复刻一次并写入 `voice_cache.json`，重复启动不会重复上传；
- 删除 `voice_cache.json` 只会导致下次重新复刻；
- GUI 保存会整体重写 `[tts.custom_voices]`（表内注释不保留），写前校验并备份 `config.toml.bak`。

---

## 调试 CLI（`admin`）

发布包**不含**调试工具；开发构建里可用：`cargo run --bin admin -- list` / `delete --voice-id <id>`，用于查看/删除百炼账号下的远程复刻音色（不读写缓存、不做前缀过滤）。

---

## 从源码构建

```powershell
cargo build --bins              # debug：kaols.exe + admin.exe
cargo run                       # 直接运行 GUI
cargo test                      # 单元测试

.\scripts\dev-run.ps1           # 开发启动：复制 config.toml 到 target\debug 并共用根目录 voice_cache.json
.\scripts\dev-run.ps1 -Glow     # 截图核对/OpenGL 兜底
.\scripts\package.ps1           # 生成 dist\kaols-<version>-win64.zip（仅 exe + README）
```

release 构建已启用 `lto = "thin"`、`strip = true`、`codegen-units = 1`；exe 图标在 `assets/kaols.ico`，由 `build.rs` 写入资源。

---

## 故障排查

- **启动提示未配置 API Key**：在「设置」页填写后保存，或编辑 `config.toml` 的 `[api] key` 后点「重新加载配置」；
- **设备打不开/找不到**：到「音频」页重新选择设备（完整名称保存到配置）；
- **GPU 初始化失败/远程桌面黑屏**：用 `renderer-glow` 变体构建（`cargo run --no-default-features --features renderer-glow`）；
- **日志**：底部日志面板与 `logs\` 文件；崩溃看 `crash.log`。

## 安全提示

- `config.toml` 里是**明文 API Key**；不要把整个程序目录发给别人，发布 zip 里不含它；
- 备份/分享目录等于分享 Key 与音色缓存，请自行评估风险；
- 所有可写文件都在 exe 同目录，不需要管理员权限；建议放在可写目录（如 `D:\Apps\kaols`）。

## 更多文档

以下文档位于源码仓库（发布包只含本 README）：

- 迁移计划与设计决策：`GUI_MIGRATION.md`
- 开发约定：`AGENTS.md`
