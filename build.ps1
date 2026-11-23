<#
.SYNOPSIS
    使用 Windows 内置 .NET Framework 4 编译器 (csc.exe) 编译 MiTV-Switch.exe（零外部 SDK 依赖）
#>
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$csc = "C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe"

if (-not (Test-Path $csc)) {
    throw "未找到 .NET Framework 4 编译器：$csc"
}

$sources = @(
    (Join-Path $root "src\NativeAndConfig.cs"),
    (Join-Path $root "src\FluentControls.cs"),
    (Join-Path $root "src\FlyoutAndSettings.cs"),
    (Join-Path $root "src\TrayApp.cs")
)
$outExe = Join-Path $root "MiTV-Switch.exe"

Write-Host "正在编译 MiTV-Switch.exe..."
& $csc /nologo /target:winexe /optimize+ /utf8output `
    /out:$outExe `
    /r:System.dll /r:System.Drawing.dll /r:System.Windows.Forms.dll `
    @sources

if ($LASTEXITCODE -ne 0) {
    throw "编译失败，退出码：$LASTEXITCODE"
}

$info = Get-Item $outExe
Write-Host "编译成功：$($info.FullName) ($($info.Length) bytes)"
