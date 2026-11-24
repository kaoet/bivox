# AGENTS.md

> 面向在本仓库工作的 AI 编码代理。构建门禁、代码/注释规范、测试、调试与发布流程全部以 [`CONTRIBUTING.md`](CONTRIBUTING.md) 为准。本文件提供阅读顺序、文档导航，以及少量跨文件硬约定（见「配置变更」）。

## 开始任务前

按顺序阅读：

1. [`CONTRIBUTING.md`](CONTRIBUTING.md) —— 强制门禁、代码/注释规范、测试、文档维护、发布流程；
2. [`docs/architecture.md`](docs/architecture.md) —— 模块划分、数据流、关键算法与边界；
3. [`docs/decisions.md`](docs/decisions.md) —— 稳定编号的 `D<n>` 设计决定（只追加、不重排）；改动涉及这些约束时逐条对照；
4. 涉及用户可见行为时读 [`README.md`](README.md)；涉及配置字段时读 [`src/config.rs`](src/config.rs) 的字段注释、`Default` 实现与 `validate`；
5. 具体源码模块顶部的 `//!` 文档注释。

## 文档导航

| 文档 | 用途 |
|---|---|
| [`README.md`](README.md) | 用户使用说明；会随发布 zip 分发，必须保持自包含 |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | 开发与贡献者的唯一规范来源（环境、构建、测试、调试、提交门禁、发布） |
| [`docs/architecture.md`](docs/architecture.md) | 当前实现的模块/算法设计 |
| [`docs/decisions.md`](docs/decisions.md) | 设计决定与理由（稳定编号 `D<n>`） |
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

- **配置不做向后兼容**：`config.toml` 的字段/段落变更时直接替换旧 schema，不保留废弃字段、别名或迁移代码。
- 原因：项目已发布，但仍维持这一约定（见 `docs/decisions.md` 的 D41）：GUI 是唯一编辑器，兼容层的成本大于收益。
- **新增/修改配置字段必须同时提供 GUI 编辑入口**（D3）：不允许只存在于 `config.toml`、只能手改的字段；字段归属哪个页签见 D22/D27/D32/D38。
- 不编写兼容/迁移测试；也不在代码、README、文档里写「不兼容旧配置」「旧字段已废弃」「请迁移」之类的说明，配置文档只描述**当前** schema。
- 旧配置按当前 schema 解析，出现未知字段就沿用现有校验直接报错，无需额外提示或迁移路径。

## 批量文本改动：不要用会解释转义的字符串

**现象**：用 PowerShell 的双引号字符串或 `@"..."@` here-string 拼正文做批量替换时，反引号
（`` ` ``）会被当成 PowerShell 转义符，正文里的行内代码会被写坏：

| 正文里写的 | 实际落盘 | 外观 |
|---|---|---|
| `` `false` `` | `` `f `` → **U+000C 换页符** | 显示成「返回  alse」 |
| `` `events.rs` `` | `` `e `` → **U+001B ESC** | 显示成「更新 <不可见>vents.rs」 |
| `` `retention_secs` `` | `` `r `` → **CR** | 一行被截成两行，第二行以「etention_secs」开头 |
| `` `n… ` ``、`` `t… ` `` | LF、TAB | 换行 / 缩进错乱，肉眼很难发现 |
| `` `chatbox` ``（非转义字母） | 反引号被吞 | 不报错，但 markdown 代码片段失效 |

**规则**：

1. 改文档或源码里的**字面文本**时，用编辑工具（`edit`）或单引号字符串（`'...'`，其中反引号是字面量）；不要用双引号/here-string 拼接含反引号、`$`、`\` 的正文。
2. 任何批量文本改动（脚本替换、文档压缩、批量改名）之后，提交前跑一次控制字符扫描——它比肉眼可靠：

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

3. 顺带检查 markdown 的**行内代码是否成对**（每行反引号数量为偶数）与是否存在以字母开头的“孤儿行”，这两条能抓出被吃掉的反引号和被 CR 截断的行。
4. 发现写坏要**修内容而不是加忽略**：还原成原本的代码片段（而不是删掉反引号或改措辞）。

## 提交

- **没有用户明确要求时，不要执行 `git commit`**。改动完成后保留在工作区并说明未提交状态，等用户明确说“提交 / commit”后再提交；也不要擅自 amend、push 或改写历史。

## 执行要求（全部引用，不在此重复）

- 构建、测试与提交门禁：按 `CONTRIBUTING.md` 第 5 节执行（包括其中对绕过检查的禁止）。
- 代码与注释规范：按 `CONTRIBUTING.md` 第 6 节；设计约束引用 `docs/decisions.md` 的 `D<n>`。
- 测试写法与验证方式：按 `CONTRIBUTING.md` 第 3、4 节；GUI/交互改动需用 egui_mcp/截图做运行期验证。
- 文档更新：按 `CONTRIBUTING.md` 第 7 节的“更新规则”同步对应文档，不要在多处重复同一事实。
- 设计冲突：以 `docs/decisions.md` 的现有决定为准；确需新增长期决定时先更新该文件（下一个 `D` 编号），再改代码与文档。

若本文件与 `CONTRIBUTING.md` 冲突，以 `CONTRIBUTING.md`（以及 `docs/decisions.md` 中对设计约束的明确记录）为准。
