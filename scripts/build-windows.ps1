$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw '此脚本只能在 Windows 上运行。'
}

Set-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
$env:npm_config_cache = Join-Path (Get-Location) '.npm-cache'

function Invoke-Checked {
    param([string] $Program, [string[]] $Arguments)

    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Program 执行失败，退出码：$LASTEXITCODE"
    }
}

Invoke-Checked npm @('ci')
Invoke-Checked npm @('run', 'prepare:binaries')
Invoke-Checked cargo @('test', '--manifest-path', 'src-tauri/Cargo.toml')
Invoke-Checked npm @('run', 'tauri', '--', 'build', '--bundles', 'nsis', '--ci')

$bundlePath = Join-Path (Get-Location) 'src-tauri\target\release\bundle\nsis'
$installer = Get-ChildItem -LiteralPath $bundlePath -Filter '*-setup.exe' -File |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
if (-not $installer) {
    throw '未找到 Windows 安装包。'
}

Write-Output "Windows 安装包：$($installer.FullName)"
Write-Output "SHA-256：$((Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256).Hash)"
