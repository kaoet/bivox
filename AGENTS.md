# AGENTS.md

> 面向在本仓库工作的 AI 编码代理。**项目规范不写在这里**：构建门禁、代码/注释规范、测试、调试与发布流程全部以 [`CONTRIBUTING.md`](CONTRIBUTING.md) 为准。本文件只提供阅读顺序与文档导航。

## 开始任务前

按顺序阅读：

1. [`CONTRIBUTING.md`](CONTRIBUTING.md) —— 强制门禁、代码/注释规范、测试、文档维护、发布流程；
2. [`docs/architecture.md`](docs/architecture.md) —— 模块划分、数据流、关键算法与边界；
3. [`docs/decisions.md`](docs/decisions.md) —— `D1`–`D32` 设计决定；改动涉及这些约束时逐条对照；
4. 涉及用户可见行为时读 [`README.md`](README.md)；涉及配置字段时读 [`config.example.toml`](config.example.toml)；
5. 具体源码模块顶部的 `//!` 文档注释。

## 文档导航

| 文档 | 用途 |
|---|---|
| [`README.md`](README.md) | 用户使用说明；会随发布 zip 分发，必须保持自包含 |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | 开发与贡献者的唯一规范来源（环境、构建、测试、调试、提交门禁、发布） |
| [`docs/architecture.md`](docs/architecture.md) | 当前实现的模块/算法设计 |
| [`docs/decisions.md`](docs/decisions.md) | 设计决定与理由（稳定编号 `D<n>`） |
| [`docs/troubleshooting/windows-temp-low-integrity.md`](docs/troubleshooting/windows-temp-low-integrity.md) | Windows 上测试写 `%TEMP%` 报 os error 5 的排查、规避与修复（DSH Low 完整性标签残留） |
| [`config.example.toml`](config.example.toml) | 配置字段与默认值的权威注释（编译期内嵌为首次启动模板） |
| [`TODO.md`](TODO.md) | 人类维护的任务与想法记录；**AI 编码代理不得修改，也应忽略其中内容** |
| `src/**/*.rs` 的 `//!` | 模块职责与关键约束 |

## 事实来源

| 信息 | 位置 |
|---|---|
| 构建/测试/提交门禁、代码规范 | [`CONTRIBUTING.md`](CONTRIBUTING.md) |
| 用户可见行为 | [`README.md`](README.md) |
| 模块职责与算法 | [`docs/architecture.md`](docs/architecture.md) |
| 设计约束与理由 | [`docs/decisions.md`](docs/decisions.md) |
| 配置字段 | [`config.example.toml`](config.example.toml) |
| 语言代码集合 | `src/language.rs` |

## 提交

- **没有用户明确要求时，不要执行 `git commit`**。改动完成后保留在工作区并说明未提交状态，等用户明确说“提交 / commit”后再提交；也不要擅自 amend、push 或改写历史。

## 执行要求（全部引用，不在此重复）

- 构建、测试与提交门禁：按 `CONTRIBUTING.md` 第 5 节执行（包括其中对绕过检查的禁止）。
- 代码与注释规范：按 `CONTRIBUTING.md` 第 6 节；设计约束引用 `docs/decisions.md` 的 `D<n>`。
- 测试写法与验证方式：按 `CONTRIBUTING.md` 第 3、4 节；GUI/交互改动需用 egui_mcp/截图做运行期验证。
- 文档更新：按 `CONTRIBUTING.md` 第 7 节的“更新规则”同步对应文档，不要在多处重复同一事实。
- 设计冲突：以 `docs/decisions.md` 的现有决定为准；确需新增长期决定时先更新该文件（下一个 `D` 编号），再改代码与文档。

若本文件与 `CONTRIBUTING.md` 冲突，以 `CONTRIBUTING.md`（以及 `docs/decisions.md` 中对设计约束的明确记录）为准。
