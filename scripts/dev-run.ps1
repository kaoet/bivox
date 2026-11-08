# 开发期启动脚本
#
# 便携模式要求 config.toml 与 exe 同目录（见 docs/decisions.md D4），
# 而 `cargo run` 的 exe 在 target\<profile>\，所以这里只把仓库根目录的
# config.toml 复制过去，并把这份副本的 [cache] file 指向仓库根的
# cache.json（相对 target\<profile>\ 向上两级）。
#
# 为什么不用符号链接（symlink）：
#   1. Windows 创建文件 symlink 需要管理员权限或开启「开发者模式」；
#   2. 更关键的是缓存写入是「临时文件 + rename」的原子写，rename 会把链接本身
#      替换成普通文件（链接断裂），所以 symlink/hardlink 都不可靠。
# 直接让配置指向根目录缓存文件，则原子写落在真实文件上，多个 profile 共用同一份缓存
# （音色复刻记录与 ASR 预编译热词 vocabulary_id 都在里面）。
#
# 用法：
#   .\scripts\dev-run.ps1            # debug，默认 wgpu 后端
#   .\scripts\dev-run.ps1 -Release   # release
#   .\scripts\dev-run.ps1 -Glow      # glow/OpenGL 后端（截图核对时用）
#   .\scripts\dev-run.ps1 -NoRun     # 只准备 target 目录，不启动 GUI（脚本自检用）

param([switch]$Release, [switch]$Glow, [switch]$NoRun)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$profileName = if ($Release) { "release" } else { "debug" }
$cargoArgs = @("build", "--bin", "kaols")
if ($Glow) { $cargoArgs += @("--no-default-features", "--features", "renderer-glow") }
if ($Release) { $cargoArgs += "--release" }

& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$targetDir = Join-Path $root "target\$profileName"

# 复制 config.toml，并把副本的 [cache] file 指到仓库根缓存
$configSource = Join-Path $root "config.toml"
$configTarget = Join-Path $targetDir "config.toml"
if (Test-Path $configSource) {
    Copy-Item $configSource $configTarget -Force

    # target\<profile>\config.toml 向上两级就是仓库根目录
    $cacheRel = "../../cache.json"
    $text = Get-Content $configTarget -Raw
    if ($text -match '(?m)^\[cache\]\s*$') {
        if ($text -match '(?m)^\s*file\s*=') {
            $text = [regex]::Replace($text, '(?m)^\s*file\s*=.*$', "file = `"$cacheRel`"")
        } else {
            $text = [regex]::Replace($text, '(?m)^\[cache\]\s*$', "[cache]`nfile = `"$cacheRel`"")
        }
    } else {
        $text = $text.TrimEnd() + "`n`n[cache]`nfile = `"$cacheRel`"`n"
    }
    Set-Content -Path $configTarget -Value $text -NoNewline
}

if ($NoRun) { exit 0 }

& (Join-Path $targetDir "kaols.exe")
exit $LASTEXITCODE
