# Windows：`%TEMP%` 写入被拒（Low 完整性标签残留）

本页记录 Windows 下 `cargo test` 只有“写 `%TEMP%` 的测试”失败（`os error 5`）的排查与处理。根因是 DSH（DeepSeek Harness）`workspace-write` 命令沙箱在工作区目录树留下的**磁盘安全状态**，与本仓库代码、`%TEMP%` 权限、杀软都无关。

## 指纹与检测

- **指纹**：`cargo test` 只有写 `%TEMP%` / 用户目录的测试失败，错误固定为
  `Os { code: 5, kind: PermissionDenied, message: "拒绝访问。" }`（os error 5）；
  工作区内写入正常；`pwsh` / `cmd` / `.NET` 自己写 `%TEMP%` 正常。
  常见失败项：`config_edit::save_*`、`log_sink::cleanup_*`、`voice::cache` 磁盘往返、
  `voice::tests` 建临时目录。
- **检测**（任一处出现 `Mandatory Label\Low Mandatory Level` 即命中）：

  ```powershell
  icacls "<工作区根>" | Select-String Mandatory
  icacls "<工作区根>\target" | Select-String Mandatory
  ```

## 根因

DSH 在 `workspace-write` 模式下会给工作区根目录写一条**可继承的 Low 完整性标签**，且是常驻状态
（会话结束、切到“完全权限”、重启都不会撤销）。之后在该目录树里构建出的 exe
（含 `target\debug\deps\*.exe`）继承 Low 标签；Windows 会让带 Low 标签的镜像以 **Low 完整性进程**
运行（即使父进程是 Medium），而 Low 进程对 Medium 的 `%TEMP%` / `%USERPROFILE%` / `%LOCALAPPDATA%`
没有写权限（MIC no-write-up）。`pwsh` / `cmd` 位于系统目录、本身没有标签，所以它们写 `%TEMP%` 正常。

## 零改动规避（不修环境也能跑测试）

```powershell
$tmp = Join-Path $PWD 'target\test-tmp'; New-Item -ItemType Directory -Force $tmp | Out-Null
$env:TEMP = $tmp; $env:TMP = $tmp
cargo test
```

`target\` 也在工作区树内（Low），Low 进程可写同标签对象。**不要**为此改测试代码放弃
`std::env::temp_dir()`。

## 一次性修复（需完全权限会话）

```powershell
icacls "<工作区根>" /setintegritylevel "(OI)(CI)M" /T
```

- 必须在非受限会话（`danger-full-access` 或普通 PowerShell 终端）执行；受限会话会报“拒绝访问”，
  不要尝试提权。
- PowerShell 里 `(OI)(CI)M` 必须加引号（否则报“术语 'OI' 不会被识别”）；`/T` 才会连同已有子项一起改。
- **不要**用 `/reset`（会重写显式 DACL）；**不要**改 `%TEMP%` 的 ACL，也不要动杀软 / 组策略。
- 修完用环境原生 `%TEMP%` 跑 `cargo test`，应全部通过。
- DSH 下次以 `workspace-write` 模式运行该工作区会**重新贴标签**（其文档化的已知限制），
  症状复发就重跑同一条命令。

## 证据（本机实测的标签形态）

```text
icacls <工作区根>
    Mandatory Label\Low Mandatory Level:(OI)(CI)(NW)        <- 根因

icacls <工作区根>\target
    Mandatory Label\Low Mandatory Level:(I)(OI)(CI)(NW)     <- 继承
```

- 失败进程 `IsRestricted=0`、`RestrictedSidCount=0`、`TEMP` 为环境原生值
  → 不是“运行中的沙箱”或受限 token；
- 同一 exe 放在工作区内启动是 Low（写 `%TEMP%` 失败），copy 到 `%TEMP%` 启动是 Medium（成功）
  → 标签跟着**文件对象**走，与路径、签名、父进程无关；
- `%TEMP%` 的 DACL 本来正常（无 Deny、当前用户 FullControl），不属于排查方向。
