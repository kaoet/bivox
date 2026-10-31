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
cargo build            # 编译
cargo test             # 全部单元测试（含本地 mock WebSocket 服务）
cargo run -- --help    # 查看 CLI 参数（-c/--config、--list-devices）
cargo run -- --list-devices   # 列出可用音频设备（无需配置文件）
cargo run --release    # 默认运行：读取 ./config.toml，实时语音识别 -> CosyVoice 合成播放
```

运行需要 `config.toml` 中的 `[api] key`（参见 `config.example.toml`）。

## 项目结构速览

- `src/main.rs`：入口，解析配置并启动编排器。
- `src/config.rs`：TOML 配置加载与校验（API Key、业务空间域名、设备、ASR/TTS 模型、VAD 参数）；clap 仅保留 `-c/--config`、`--list-devices`。
- `src/audio/`：`cpal` 采集/播放、设备枚举、重采样、本地能量 VAD 门控。
- `src/client/`：
  - `qwen_asr.rs`：百炼 Qwen Audio 实时双向流式识别（run-task 协议）。
  - `cosyvoice.rs`：CosyVoice 流式语音合成（run-task 协议，单连接复用）。
  - `ws.rs`：鉴权建连、握手、Ping 保活、断线指数退避重连的公共骨架。
- `src/pipeline/`：`chunker.rs` 流式文本分块，`orchestrator.rs` 编排 ASR -> 分块 -> TTS。

## 代码约定

- 注释与终端提示语用中文，标识符用英文；提交信息简洁描述“做了什么、为什么”。
- 涉及 WebSocket 协议、重连、VAD 等逻辑时，测试使用本地 `tokio` mock 服务，不依赖真实百炼网络。
- 不要提交 `config.toml`（已被 `.gitignore` 忽略）；密钥只放在 `[api] key` 中，不再读取环境变量。
