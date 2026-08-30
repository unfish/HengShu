param(
    [Parameter(Mandatory = $true)][string]$Source,
    [Parameter(Mandatory = $true)][string]$Tool,
    [Parameter(Mandatory = $true)][string]$Output
)
$ErrorActionPreference = "Stop"
$sourcePath = (Resolve-Path $Source).Path
$projectRoot = if (Test-Path $sourcePath -PathType Container) { $sourcePath } else { Split-Path $sourcePath -Parent }
while ($projectRoot -and -not (Test-Path (Join-Path $projectRoot "衡枢.toml"))) {
    $parent = Split-Path $projectRoot -Parent
    if ($parent -eq $projectRoot) { break }
    $projectRoot = $parent
}
if (-not (Test-Path (Join-Path $projectRoot "衡枢.toml"))) { throw "找不到衡枢.toml" }
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$name = if (Test-Path $sourcePath -PathType Container) { Split-Path $sourcePath -Leaf } else { [IO.Path]::GetFileNameWithoutExtension($sourcePath) }
$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
$dest = Join-Path $Output "windows-$arch"
$build = Join-Path $repoRoot "target\release"
New-Item -ItemType Directory -Force $dest | Out-Null
Push-Location $repoRoot
try {
    cargo build --release --bin 衡枢
    if ($LASTEXITCODE -ne 0) { throw "CLI Release 构建失败" }
    cargo build --release --features gui --bin 衡枢GUI
    if ($LASTEXITCODE -ne 0) { throw "GUI Release 构建失败" }
} finally { Pop-Location }
$cliHost = Join-Path $build "衡枢.exe"
$guiHost = Join-Path $build "衡枢GUI.exe"
$cli = Join-Path $dest "$name-cli.exe"
$gui = Join-Path $dest "$name-gui.exe"
& $cliHost 打包 $sourcePath --命令行 --工具 $Tool -o $cli
if ($LASTEXITCODE -ne 0) { throw "CLI 载荷打包失败" }
$env:HENGSHU_GUI_BINARY = $guiHost
try {
    & $cliHost 打包 $sourcePath --界面 --工具 $Tool -o $gui
    if ($LASTEXITCODE -ne 0) { throw "GUI 载荷打包失败" }
} finally { Remove-Item Env:HENGSHU_GUI_BINARY -ErrorAction SilentlyContinue }
& $cliHost 验证包 $cli
if ($LASTEXITCODE -ne 0) { throw "CLI 包验证失败" }
& $cliHost 验证包 $gui
if ($LASTEXITCODE -ne 0) { throw "GUI 包验证失败" }
Push-Location $repoRoot
try { cargo metadata --format-version 1 --locked | Out-File -Encoding utf8 (Join-Path $dest "DEPENDENCIES.cargo-metadata.json") }
finally { Pop-Location }
@(
    "platform=windows", "architecture=$arch", "package=$name", "tool=$Tool",
    "source=$sourcePath", "cli=$name-cli.exe", "gui=$name-gui.exe"
) | Set-Content (Join-Path $dest "BUILD-MANIFEST")
Get-ChildItem -File $dest | Where-Object { $_.Name -ne "SHA256SUMS" } | ForEach-Object {
    "{0}  {1}" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.Name
} | Sort-Object | Set-Content (Join-Path $dest "SHA256SUMS")
Write-Host "平台产物已生成：$dest"
