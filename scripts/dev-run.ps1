# 开发期启动脚本
#
# 便携模式要求 config.toml 与 exe 同目录（GUI_MIGRATION.md D4），
# 而 `cargo run` 的 exe 在 target\<profile>\，所以这里先把仓库根目录的
# config.toml / voice_cache.json 复制过去，再启动 GUI。
#
# 用法：
#   .\scripts\dev-run.ps1            # debug，默认 wgpu 后端
#   .\scripts\dev-run.ps1 -Release   # release
#   .\scripts\dev-run.ps1 -Glow      # glow/OpenGL 后端（截图核对时用）

param([switch]$Release, [switch]$Glow)

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
foreach ($name in @("config.toml", "voice_cache.json")) {
    $source = Join-Path $root $name
    if (Test-Path $source) {
        Copy-Item $source (Join-Path $targetDir $name) -Force
    }
}

& (Join-Path $targetDir "kaols.exe")
exit $LASTEXITCODE
