# kaols — 超低延迟实时语音识别 / 语音合成（Windows GUI）

基于阿里云百炼 **Qwen-Audio** 的实时语音识别（ASR）与语音合成（TTS）桌面客户端，面向实时对话、VRChat 等低延迟场景。

全双工流式流水线：麦克风采集 → 流式 ASR → 按标点/语义分块 → 流式 TTS → 平滑播放，无需等整句说完。

- 纯 GUI（egui/eframe），**便携模式**：配置、缓存、日志都在 exe 同目录，整个文件夹拷走即用；
- **单实例**：同一目录只允许开一个窗口，重复启动会激活已有窗口；
- 本地 VAD 门控、实时字幕（语言代码 + TTS 进度分色）、多语言自定义（复刻）音色。

**系统要求**：Windows 10/11 x64、可用的百炼 API Key、联网。发布包不含安装器，解压即用。

---

## 快速开始

1. 解压发布包（`kaols.exe` + `README.md`）到一个**可写目录**（例如 `D:\Apps\kaols`）；
2. 双击 `kaols.exe`：首次启动会在 exe 同目录生成 `config.toml`，并自动落在「设置」页；
3. 在「设置」页填入百炼 API Key（[控制台](https://bailian.console.aliyun.com/)），点「保存 API Key」；
4. 回到「实时」页点「开始」，对着麦克风说话即可看到字幕并听到合成语音。

> 单实例行为：第二次启动会尝试把已有窗口恢复到前台并退出，不会抢麦克风。
> 不同目录的两份便携副本可以同时运行。

### 便携目录

```text
kaols.exe
config.toml            # 首次启动生成；含 API Key，注意保密
config.toml.bak        # 每次在 GUI 中写回配置前的备份
voice_cache.json       # 自定义音色复刻缓存（模型 → 音频指纹 → voice_id）
logs\kaols-YYYY-MM-DD.log  # 按天日志，保留最近 7 天
crash.log              # 崩溃信息（如有）
```

程序只读 **exe 所在目录**的 `config.toml`，不读 `%APPDATA%`、当前工作目录或环境变量。
配置里的相对路径（缓存、样例音频）以 `config.toml` 所在目录为基准。

---

## 界面

| 页签 / 区域 | 说明 |
|---|---|
| 实时 | 「开始 / 停止」、本地 VAD 指示器、实时字幕（最新句在最上，语言代码在左）；支持单句复制；底部输入框可打字朗读（Enter 提交） |
| 音频 | 选择输入 / 输出设备（完整设备名精确匹配）；改动后立即保存并热更新 |
| 语音合成 | 顶部选择「当前音色」；下方编辑自定义音色（显示名、键名、多语言声线样本、每个样本的语速滑条、拖拽排序）；右上角「保存 / 放弃更改」 |
| 设置 | 便携路径、API Key、重新加载配置、打开程序目录、只读配置概览（域名/模型/语言/音色） |
| 底部日志 | 运行日志（内存保留最近 5000 行，面板显示最近 500 行），按级别着色，高度可拖拽 |

窗口右上角显示程序版本号。日志面板与 `logs\` 文件同时记录，适合排查连接/识别/合成问题。

### 当前音色

「语音合成」页顶部 Radio 二选一，未选中的一侧控件禁用：

- **现有音色**：输入框，填内置音色名（如 `qiaoxiaojiao_v3.1`）或任意百炼 voice_id；
- **自定义音色**：下拉选择 `[tts.custom_voices]` 里的条目（写入 `custom:<键名>`）。

改动后点页面右上角 **保存** 一次写回 `config.toml`；**放弃更改** 同时还原当前音色与下方音色编辑草稿。
运行中保存会按「配置热更新」自动用新配置重启实时管线。

内置音色没有可按模型查询的列表 API（官方只有文档表：[Qwen-Audio-TTS 音色列表](https://help.aliyun.com/zh/model-studio/qwen-audio-tts-voice-list)），因此用输入框填写 voice id。

### 实时页打字输入

「实时」页字幕列表下方有一个输入框（支持多行，**Shift+Enter** 换行）：输入文字后按 **Enter** 或点「朗读」，程序会用当前音色把这段文字读出来。

- 仅「运行中」时可提交；未开始时可以先输入，提交时会提示先点「开始」。
- 单次输入上限 **500 字符**；空白不会提交。
- 朗读是**排队**的：当前已排队/正在播放的音频会照常播完，你输入的句子排在后面依次朗读，不会打断正在播放的声音。
- 语言沿用当前对话的语言判定（例如一直在用日语交流，输入“东京”仍按日语读）。
- 如果提交时实时识别的那一句还没结束，该句只保留已经送去合成的前缀进字幕，剩余识别内容不会补读。
- 每句输入都会出现在字幕历史里（不区分是打字还是语音）。

---

## 配置说明

`config.toml` 首次启动自动生成，**完整注释见文件本身**。常用字段：

```toml
[api]
key = "sk-..."                 # 必填
# workspace_host = "ws-xxxx.cn-beijing.maas.aliyuncs.com"  # 业务空间专属域名（纯 host）

[asr]
# model = "qwen-audio-3.1-asr-flash-streaming"
languages = ["zh", "ja"]       # 有序；只写一个 = 强提示该语言；整份列表作为 ASR 提示

[tts]
# model = "qwen-audio-3.1-tts-flash"
voice = "qiaoxiaojiao_v3.1"    # 系统音色名 / 复刻 voice_id / custom:<键名>
# cache_file = "voice_cache.json"

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

- `[api] workspace_host` 只填**纯 host**，不要带 `wss://`、路径或端口；省略该字段则使用百炼通用域名。
- `[audio]` 设备名按**完整名称精确匹配**：设备改名/被拔掉会明确报错，不会静默回退到默认设备。
- `[vad] debug = true` 可打印每个音频块的 VAD 电平与发送决策，用于现场标定阈值（仅排障用）。
- 其它字段（`[asr] model`、`[tts] model`、`[chunking] words` 等）暂不支持图形化编辑，手改后点「设置 → 重新加载配置」。

### 支持的语言

`[asr] languages` 使用 ASR 语言代码（30 种）：

```text
zh en ja ko vi th id ms tl hi ar fr de es pt ru it nl sv da fi no el pl cs hu ro bg hr sk
```

自定义音色样本的 `language` 使用**声音复刻**语言代码（16 种）：

```text
zh en fr de ja ko ru pt th id vi es it ms fil ar
```

- ASR 的 `tl`（菲律宾语）在复刻侧自动按 `fil` 处理；
- 常见非中英语种（ja/ko/fr/de/ru/es/pt/it/th/vi/id/ms/fil/ar）合成时会附带内置语种指令；不在复刻语言列表内的语言合成时按 `other` 处理，自定义音色会回退到默认声线；
- 列表顺序有意义：第一个是默认语言，本地判定也以完整列表为范围。

---

## 自定义（复刻）音色

一个音色可以有多份不同语言的声线样本，合成时按本句判定的语言选样本（缺该语言回退第一个样本）：

```toml
[tts.custom_voices.edgar]
name = "异环 - 埃德嘉"

[[tts.custom_voices.edgar.samples]]
language = "zh"
audio = "samples/edgar-zh.wav"
rate = 1.0                     # 可选合成语速，默认 1.0，范围 0.5–2.0

[[tts.custom_voices.edgar.samples]]
language = "ja"
audio = "samples/edgar-ja.wav"
rate = 1.0
```

- 样本音频：清晰单人、≥5 秒（推荐 10–20 秒，单条建议 ≤60 秒）、≤10 MB，支持 wav/mp3/m4a；
- `rate` 是**样本级**语速（同一音色不同语言的声线可不同）：默认 `1.0`，范围 `0.5`–`2.0`；合成时用本句语言所选样本的语速，缺该语言回退第一个样本时用该样本的语速；内置音色/普通 voice_id 固定 `1.0`；
- GUI 每个样本有「语速」滑条（步长 0.05）与「重置」（恢复默认 1.0×），随「保存」一起写回；运行中保存会按「配置热更新」自动重启实时管线；
- 语速不参与复刻/缓存：改 `rate` 不会重新上传或复刻样例音频；
- 每个样本按（TTS 模型, 音频内容指纹）复刻一次并写入 `voice_cache.json`，重复启动不会重复上传；
- 删除 `voice_cache.json` 只会导致下次重新复刻；同一段音频在不同 TTS 模型下分别缓存；
- 若百炼账号里的复刻音色被删除，下次开始会话时会自动检测并重新复刻，一般无需手工处理缓存；
- GUI 保存会整体重写 `[tts.custom_voices]`（**表内注释不保留**），写前校验并备份 `config.toml.bak`；
- GUI 选音频时：文件在程序目录内则保存相对路径，目录外则保存绝对路径（可能影响便携性，界面会提示）。

---

## 故障排查

- **启动提示未配置 API Key**：在「设置」页填写后保存，或编辑 `config.toml` 的 `[api] key` 后点「重新加载配置」。
- **设备打不开/找不到**：到「音频」页重新选择设备（完整名称会保存到配置）；设备改名或被拔掉时不会自动回退默认设备。
- **连接/鉴权失败**：检查 Key 是否有效、`[api] workspace_host` 是否填对（业务空间专属域名只填 host）；底部日志面板有服务端错误码。
- **GPU 初始化失败 / 远程桌面黑屏**：发布版默认使用 wgpu（DX12）；可从源码用 `renderer-glow`（OpenGL）变体构建兜底，构建方式见 `CONTRIBUTING.md`。
- **自定义音色失败**：检查样例音频路径、时长/大小/格式，以及账号下音色是否仍存在；日志里会标出是哪一步失败。
- **日志与崩溃**：底部日志面板与 `logs\` 目录；崩溃信息见 `crash.log` 和界面顶部红色错误条。
- **双击没反应**：可能已有实例在运行（会尝试激活已有窗口）；也检查程序是否被安全软件拦截。

## 安全提示

- `config.toml` 里是**明文 API Key**；不要把整个程序目录发给别人，发布 zip 里不含它；
- 备份/分享目录等于分享 Key 与音色缓存，请自行评估风险；
- 所有可写文件都在 exe 同目录，不需要管理员权限；建议放在可写目录（如 `D:\Apps\kaols`）。

---

## 开发、测试与发布

从源码构建、运行测试、调试（含 egui_mcp 运行期观测）与发布打包的完整说明见 [`CONTRIBUTING.md`](CONTRIBUTING.md)。

设计与实现文档（均位于源码仓库）：

| 文档 | 内容 |
|---|---|
| [docs/architecture.md](docs/architecture.md) | 模块划分、数据流与关键算法 |
| [docs/decisions.md](docs/decisions.md) | `D1`–`D24` 设计决定与理由 |
| [AGENTS.md](AGENTS.md) | AI 编码代理的阅读入口 |

百炼音色调试 CLI `admin` 仅用于本地开发调试（列出/删除账号下的远程复刻音色），**不随发布包分发**，用法见 [`CONTRIBUTING.md`](CONTRIBUTING.md)。

> 发布包只包含本 `README.md`，上述文档不会随 zip 分发。
