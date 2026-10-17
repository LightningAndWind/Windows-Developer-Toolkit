# 生成自包含发布包：devkit.exe + 便携版 Git (MinGit) + Win32-OpenSSH，解压即用。
# 用法：powershell -ExecutionPolicy Bypass -File package.ps1 [-SkipBuild] [-OutDir dist]
# 说明：工具放在 exe 同级 tools\ 下，应用按「tools\ → 数据目录 tools\ → PATH」顺序解析。

param(
    [switch]$SkipBuild,
    [string]$OutDir = "dist"
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$root = $PSScriptRoot
$appDir = Join-Path $root 'devkit-windows'
$dist = Join-Path $root $OutDir
$stage = Join-Path $dist 'devkit-win64'

# ---- 1. 构建 release（需要 w64devkit 在 PATH，见 devkit-windows\.cargo\config.toml） ----
if (-not $SkipBuild) {
    Push-Location $appDir
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw 'cargo build --release 失败' }
    } finally { Pop-Location }
}
$exe = Join-Path $appDir 'target\release\devkit.exe'
if (-not (Test-Path $exe)) { throw "未找到 $exe（先去掉 -SkipBuild 完成构建）" }

# ---- 2. 定位最新发布资产（/latest 无命中时翻 /releases 列表，兼容 prerelease-only 仓库） ----
function Get-AssetUrl([string]$Api, [scriptblock]$Match) {
    foreach ($ep in @("$Api/latest", "$Api`?per_page=20")) {
        try { $rels = Invoke-RestMethod -Uri $ep -Headers @{ 'User-Agent' = 'devkit-package' } } catch { continue }
        $list = if ($rels -is [array]) { $rels } else { @($rels) }
        foreach ($rel in $list) {
            foreach ($a in $rel.assets) {
                if (& $Match $a.name) { return $a.browser_download_url }
            }
        }
    }
    throw "未找到发布资产：$Api"
}

$gitUrl = Get-AssetUrl 'https://api.github.com/repos/git-for-windows/git/releases' { param($n) $n -like 'MinGit-*-64-bit.zip' }
$sshUrl = Get-AssetUrl 'https://api.github.com/repos/PowerShell/Win32-OpenSSH/releases' { param($n) $n -eq 'OpenSSH-Win64.zip' }

# ---- 3. 下载 + 解压到 stage\tools ----
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path (Join-Path $stage 'tools') | Out-Null
$tmp = Join-Path $dist '.download'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

function Expand-Zip([string]$Url, [string]$ZipName) {
    $zip = Join-Path $tmp $ZipName
    Write-Host "下载 $Url"
    Invoke-WebRequest -Uri $Url -OutFile $zip -UserAgent 'devkit-package'
    $ex = Join-Path $tmp ($ZipName + '.x')
    if (Test-Path $ex) { Remove-Item -Recurse -Force $ex }
    Expand-Archive -LiteralPath $zip -DestinationPath $ex -Force
    return $ex
}

# MinGit：cmd/git.exe 或 mingw64/bin/git.exe，向上取 MinGit 根
$ex = Expand-Zip $gitUrl 'MinGit.zip'
$gitSrc = $null
foreach ($rel in @('cmd\git.exe', 'mingw64\bin\git.exe')) {
    $p = Join-Path $ex $rel
    if (Test-Path $p) { $gitSrc = Split-Path (Split-Path $p -Parent) -Parent; break }
}
if (-not $gitSrc) {
    $gitSrc = Get-ChildItem $ex -Recurse -Filter git.exe |
        Where-Object { $_.DirectoryName -like '*cmd*' } |
        Select-Object -First 1 | ForEach-Object { Split-Path (Split-Path $_.FullName -Parent) -Parent }
}
if (-not $gitSrc) { throw 'MinGit 压缩包内未找到 git.exe' }
Move-Item $gitSrc (Join-Path $stage 'tools\git')
Write-Host "MinGit    -> tools\git"

# Win32-OpenSSH：ssh.exe 所在目录
$ex2 = Expand-Zip $sshUrl 'OpenSSH-Win64.zip'
$sshExe = Get-ChildItem $ex2 -Recurse -Filter ssh.exe | Select-Object -First 1
if (-not $sshExe) { throw 'OpenSSH 压缩包内未找到 ssh.exe' }
Move-Item (Split-Path $sshExe.FullName -Parent) (Join-Path $stage 'tools\ssh')
Write-Host "OpenSSH   -> tools\ssh"

# ---- 4. 组装 ----
Copy-Item $exe $stage
@'
devkit 运行工具说明
==================
应用按以下顺序解析外部工具（git / ssh / ssh-keygen）：
1. 环境变量 DEVKIT_GIT / DEVKIT_SSH / DEVKIT_SSH_KEYGEN
2. exe 同级 tools\ 目录（本发布包自带）
3. 数据目录 tools\（应用内 设置→通用→运行工具 一键安装位置）
4. 系统 PATH

本包内置：
- MinGit（Git for Windows，GPLv2）https://gitforwindows.org
- Win32-OpenSSH（BSD 系许可）https://github.com/PowerShell/Win32-OpenSSH
按各自许可证随包分发；源码与许可文本见上游项目。
'@ | Set-Content -Encoding UTF8 (Join-Path $stage 'THIRD-PARTY-NOTICES.txt')

# ---- 5. 压缩 ----
$zipOut = Join-Path $dist ('devkit-win64-' + (Get-Date -Format 'yyyyMMdd') + '.zip')
if (Test-Path $zipOut) { Remove-Item -Force $zipOut }
Compress-Archive -Path $stage -DestinationPath $zipOut
Write-Host "完成：$zipOut"
