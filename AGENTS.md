# AGENTS.md

本文件面向在本仓库中工作的 AI 编码代理（Claude Code / Codex / Cursor 等）。人类贡献者也建议遵守同样的约定。

## 强制规则：每次 `git commit` 前必须 `cargo fmt`

**在创建任何 git 提交之前，必须先运行 `cargo fmt`。** 这是硬性要求，没有例外：

```bash
cargo fmt          # 1. 格式化整个项目（不是只格式化本次改动的文件）
cargo fmt --check  # 2. 验证全项目无格式差异，退出码必须为 0
cargo build        # 3. 确认编译通过
cargo test         # 4. 确认测试通过
git add -A
git commit -m "..."  # 5. 以上全部通过后才允许提交
```

约束：

- 不允许在 `cargo fmt --check` 失败（退出码非 0）时提交。
- 不允许用 `git commit --no-verify` 之类的参数绕过检查。
- 如果格式化顺带改到了与本次任务无关的文件，必须把这些纯格式改动一并提交或单独提交，不能为了让 diff 好看而撤销它们从而导致全项目 `cargo fmt --check` 失败。
- 提交前用 `git diff --check` 再确认没有空白符错误。

## 常用命令

```bash
cargo build              # 编译
cargo test               # 全部单元测试（含本地 mock WebSocket / HTTP 服务）
cargo run -- --help      # 查看 CLI（全局 -c/--config 与 run/devices/voices 子命令）
cargo run -- devices     # 列出可用音频设备（无需配置文件）
cargo run -- voices list # 列出百炼账号下已上传的自定义音色
cargo run -- voices delete --voice-id <id>
cargo run -- voices delete --voice <配置名> [--language <code>]
cargo run -- voices delete-all [-y]
cargo run -- voices enroll [配置名] [--language <code>]
cargo run --release      # 默认运行：读取 ./config.toml，实时语音识别 -> Qwen-Audio-TTS 合成播放
```

运行需要 `config.toml` 中的 `[api] key`（参见 `config.example.toml`）。

## 项目结构速览

- `src/main.rs`：入口，按子命令分发（默认 `run`）。
- `src/config.rs`：TOML 配置加载与校验（API Key、业务空间域名、设备、ASR/TTS 模型、
  `[asr] languages`、多语言自定义音色样本、VAD 参数）；
  clap 定义 `-c/--config` 与 `run` / `devices` / `voices` 子命令。
- `src/language.rs`：语言代码集合、合成 `instruction` 映射、ASR 文本脚本统计与
  `LanguageTracker`（强证据 + 粘性 + `han >= 6` 切中文等规则）。
- `src/audio/`：`cpal` 采集/播放、设备枚举、重采样、本地能量 VAD 门控。
- `src/client/`：
  - `qwen_asr.rs`：百炼 Qwen Audio 实时双向流式识别（run-task 协议；透传 `language_hints`，>4 截断警告）。
  - `qwen_tts.rs`：Qwen-Audio-TTS 流式语音合成（run-task 协议，单连接复用；
    每个句子按上游给出的 voice / language 填 `language_hints` + `instruction`）。
  - `ws.rs`：鉴权建连、握手、Ping 保活、断线指数退避重连的公共骨架。
- `src/pipeline/`：`chunker.rs` 流式文本分块，`orchestrator.rs` 编排 ASR -> 语言判定 -> 分块 -> TTS。
- `src/voice/`：自定义（复刻）音色。
  - `api.rs`：调百炼 HTTP 接口（文件上传、音色复刻/查询/列表/删除），不依赖 SDK。
  - `cache.rs`：`voice_cache.json`（模型 -> 音频 SHA256 -> voice_id）读写。
  - `cli.rs`：`kaols voices list/delete/delete-all/enroll` 实现。
  - `mod.rs`：启动时解析 `[tts] voice`，并行注册全部语言样本，返回「语言 -> voice_id」映射。

## 代码约定

- 注释与终端提示语用中文，标识符用英文；提交信息简洁描述“做了什么、为什么”。
- 涉及 WebSocket 协议、重连、VAD、音色 HTTP 接口等逻辑时，测试使用本地 `tokio` mock 服务（`src/voice/mock_http.rs` 是纯手写的 HTTP mock），不依赖真实百炼网络。
- 自定义音色结构：`[[tts.custom_voices.<名>.samples]]` 是**有序数组**，`samples[0]` 是默认声线；
  每个样本按（模型, 音频内容哈希）单独复刻/缓存；合成时按判定语言选样本，缺语言回退 `samples[0]`。
- 语言策略：`[asr] languages` 有序（首个默认），只截断传给 ASR API 的 `language_hints`（≤4），
  **本地判定始终使用完整列表**；判定规则见 `language.rs`（假名/谚文强证据、纯汉字 ≥6 切中文、
  拉丁需主导、短文本粘性），不要改成"等整句再判"（会破坏低延迟）。
- 合成时始终传 `language_hints`（目标语言），非中英语种附带内置 `instruction`；这两者与复刻样本语种无关。
- 自定义音色缓存是「模型 -> 样例音频内容 SHA256 -> voice_id」两层结构，**不要让每次合成都重新上传/复刻**；同一音频在不同模型下分别缓存，切换模型不应覆盖其它模型的记录。
- 项目尚未发布：配置/缓存文件里不要引入版本号，也不要写旧格式兼容代码；文件格式变化时让旧文件解析失败并自动备份重建即可。不要为“旧配置/旧数据不再兼容”专门写测试，也不要在文档中描述这类不兼容/迁移说明。
- 不要提交 `config.toml`（已被 `.gitignore` 忽略）；密钥只放在 `[api] key` 中，不再读取环境变量。
- `voice_cache.json` 是本机状态（也被 `.gitignore` 忽略），不要提交。
