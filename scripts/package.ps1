# 发布打包脚本（M5）
#
# 产物：dist\kaols-<version>-win64.zip
#   只包含 kaols.exe + README.md（不含 config.toml / config.example.toml / 调试工具 / 缓存 / 日志）。
#
# 用法：
#   .\scripts\package.ps1             # 构建 release 并打包
#   .\scripts\package.ps1 -NoBuild    # 跳过 cargo build（用现有 target\release\kaols.exe 打包）

param([switch]$NoBuild)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$versionLine = Select-String -Path "Cargo.toml" -Pattern '^version = "([^"]+)"' | Select-Object -First 1
if (-not $versionLine) { throw "Cargo.toml 里找不到 package version" }
$version = $versionLine.Matches[0].Groups[1].Value

if (-not $NoBuild) {
    & cargo build --release --bin kaols
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

$exe = Join-Path $root "target\release\kaols.exe"
if (-not (Test-Path $exe)) {
    throw "找不到 $exe；先执行 cargo build --release --bin kaols 或去掉 -NoBuild"
}

$dist = Join-Path $root "dist"
$stage = Join-Path $dist "kaols-$version-win64"
$zip = Join-Path $dist "kaols-$version-win64.zip"

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage -Force | Out-Null

Copy-Item $exe $stage
Copy-Item (Join-Path $root "README.md") $stage

if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip -CompressionLevel Optimal

$sizeMb = [math]::Round((Get-Item $zip).Length / 1MB, 2)
$exeMb = [math]::Round((Get-Item $exe).Length / 1MB, 2)
Write-Host "打包完成：$zip"
Write-Host "  版本：$version"
Write-Host "  kaols.exe：$exeMb MB"
Write-Host "  zip：$sizeMb MB"
Write-Host "  内容：kaols.exe + README.md（无 config.toml、无密钥）"
