# AGENTS.md

> 面向在本仓库工作的 AI 编码代理。构建门禁、代码/注释规范、测试、调试与发布流程全部以 [`CONTRIBUTING.md`](CONTRIBUTING.md) 为准。本文件提供阅读顺序、文档导航，以及少量跨文件硬约定（见下文各节）。

## 开始任务前

按顺序阅读：

1. [`CONTRIBUTING.md`](CONTRIBUTING.md) —— 强制门禁、代码/注释规范、测试、文档维护、发布流程；
2. [`docs/architecture.md`](docs/architecture.md) —— 模块划分、数据流、关键算法与边界；
3. [`docs/decisions.md`](docs/decisions.md) —— `D<n>` 编号的设计约束（被替代时先改引用、再删条目；编号作废不回填）；改动涉及这些约束时逐条对照；
4. 涉及用户可见行为时读 [`README.md`](README.md)；涉及配置字段时读 [`src/config.rs`](src/config.rs) 的字段注释、`Default` 实现与 `validate`；
5. 具体源码模块顶部的 `//!` 文档注释。

## 文档导航

| 文档 | 用途 |
|---|---|
| [`README.md`](README.md) | 用户使用说明；会随发布 zip 分发，必须保持自包含 |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | 开发与贡献者的唯一规范来源（环境、构建、测试、调试、提交门禁、发布） |
| [`docs/architecture.md`](docs/architecture.md) | 当前实现的模块/算法设计 |
| [`docs/decisions.md`](docs/decisions.md) | 设计约束与理由（`D<n>` 编号，被替代的条目会被删除） |
| [`docs/troubleshooting/windows-temp-low-integrity.md`](docs/troubleshooting/windows-temp-low-integrity.md) | Windows 上测试写 `%TEMP%` 报 os error 5 的排查、规避与修复（DSH Low 完整性标签残留） |
| [`src/config.rs`](src/config.rs) | 配置字段、默认值与校验的唯一完整说明（字段注释 + `Default`/常量 + `validate`） |
| [`TODO.md`](TODO.md) | 人类维护的任务与想法记录；**AI 编码代理不得修改，也应忽略其中内容，不要包含到AI生成的git commit中** |
| `src/**/*.rs` 的 `//!` | 模块职责与关键约束 |

## 事实来源

| 信息 | 位置 |
|---|---|
| 构建/测试/提交门禁、代码规范 | [`CONTRIBUTING.md`](CONTRIBUTING.md) |
| 用户可见行为 | [`README.md`](README.md) |
| 模块职责与算法 | [`docs/architecture.md`](docs/architecture.md) |
| 设计约束与理由 | [`docs/decisions.md`](docs/decisions.md) |
| 配置字段与默认值 | [`src/config.rs`](src/config.rs) |
| 语言代码集合 | `src/language.rs` |

## 配置变更

### 配置不做向后兼容

- `config.toml` 的字段/段落变更时直接替换旧 schema，不保留废弃字段、别名或迁移代码。
- 旧配置按当前 schema 解析，出现未知字段就沿用现有校验直接报错，无需额外提示或迁移路径。
- 原因：项目已发布，但仍维持这一约定（见 `docs/decisions.md` 的 D41）：GUI 是唯一编辑器，兼容层的成本大于收益。

### 所有配置项可 GUI 编辑

新增或修改配置字段时必须同时提供 GUI 编辑入口：不允许只存在于 `config.toml`、只能手改的字段；字段归属哪个页签见 D22/D27/D32/D38。

## 删除要干净：不留「无/没有」式说明与断言

删除功能、代码、配置时，直接移除对应的代码、注释、文档、测试。除非明确要求验证某项**不应存在**（协议/安全约束），否则不写“无 / 没有 / 不含 / 预留 / 暂未使用 / 目前无调用方”这类反向注释或反向断言。

## 批量改写文件

**改文件内容只用 `edit` / `write` 工具**——它们按字面量写入，不做任何插值。shell 只用于只读检查（`git grep`、行数统计、控制字符扫描）。

需要脚本批量改写时用 **Python**（DSH 自带的解释器路径见 `load_workspace_dependencies`），不要用 shell 做字符串替换。这类事故已经发生过两次：PowerShell 双引号会把反引号当转义符（`` `f `` → U+000C、`` `e `` → U+001B、`` `r `` → CR 截断行、普通单词前的反引号被吞），`$pair[0]` 会把字符串当成字符数组（`Replace('-',' ')` 清空了全文的列表项）；bash 同类陷阱是双引号里的反引号会被当命令替换。

脚本必须同时满足四条：

1. **先断言后写**：逐个定位锚点（行首前缀、`## D<n>`、标题行），任一断言失败就 `sys.exit(1)` 退出，绝不写半成品；
2. **dry-run 优先**：先打印 planned changes（行号 + 旧/新内容前 40 字符），确认无误再加 `--apply`；
3. **写前备份、写后校验**：`shutil.copy2` 备份原文件；写完重新读取并校验（行数合理、锚点仍在、无控制字符），不一致立即用备份还原；
4. **显式编码与换行**：`encoding='utf-8'`、`newline='\n'`，仓库统一 LF、不带 BOM。

任何批量改动（脚本替换、文档压缩、批量改名）之后，提交前再跑一遍控制字符扫描：

```powershell
foreach ($rel in (git ls-files | Where-Object { $_ -notmatch '\.(png|ico|onnx|zip|wav|jpg)$' })) {
  $t = [System.IO.File]::ReadAllText((Join-Path (Get-Location) $rel))
  for ($i = 0; $i -lt $t.Length; $i++) {
    $c = [int]$t[$i]
    if (($c -lt 32 -and $c -ne 9 -and $c -ne 10 -and $c -ne 13) -or $c -eq 0xFEFF -or $c -eq 0x200B) {
      "$rel : U+{0:X4} @ {1}" -f $c, $i
    }
  }
}
```

顺带检查 markdown 的**行内代码是否成对**（每行反引号数量为偶数）与是否存在以字母开头的“孤儿行”——这两条能抓出被吞掉的反引号和被 CR 截断的行。写坏了要**修内容而不是加忽略**：还原成原本的代码片段，而不是删掉反引号或改措辞。

## 提交

- **没有用户明确要求时，不要执行 `git commit`**。改动完成后保留在工作区并说明未提交状态，等用户明确说“提交 / commit”后再提交；也不要擅自 amend、push 或改写历史。

## 执行要求（全部引用，不在此重复）

- 构建、测试与提交门禁：按 `CONTRIBUTING.md` 第 5 节执行（包括其中对绕过检查的禁止）。
- 代码与注释规范：按 `CONTRIBUTING.md` 第 6 节；设计约束引用 `docs/decisions.md` 的 `D<n>`。
- 测试写法与验证方式：按 `CONTRIBUTING.md` 第 3、4 节；GUI/交互改动需用 egui_mcp/截图做运行期验证。
- 文档更新：按 `CONTRIBUTING.md` 第 7 节的“更新规则”同步对应文档，不要在多处重复同一事实。
- 设计冲突：以 `docs/decisions.md` 的现有决定为准。**新功能默认不新增 `D` 编号**：只有「不记录就会被未来改动破坏」的约束才写进去，写之前过 `CONTRIBUTING.md` 第 7 节的准入三问；「怎么做」写 `docs/architecture.md`，「这次为什么这样改」留在 commit message。

若本文件与 `CONTRIBUTING.md` 冲突，以 `CONTRIBUTING.md`（以及 `docs/decisions.md` 中对设计约束的明确记录）为准。
