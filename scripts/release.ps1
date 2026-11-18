# Baut ein Vyre-Release nach dist\:
#   vyre-setup-<version>.exe   der Installer (Inno Setup)
#   vyre-setup.exe             derselbe mit festem Namen für Download-Links
#   update.json                was Vyres Update-Prüfung liest (Version, Installer, SHA-256)
#
# Aufruf:  powershell -File scripts\release.ps1 [-Notes "Was ist neu"] [-Publish]
# -Publish legt das GitHub-Release v<version> in Colniro/vyre an (gh muss angemeldet sein).
# Vyre liest https://github.com/Colniro/vyre/releases/latest/download/update.json.

param([string]$Notes = "", [switch]$Publish)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "Vyre $version"

Write-Host "Baue (release) ..."
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build fehlgeschlagen" }

$iscc = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 nicht gefunden (winget install JRSoftware.InnoSetup)" }

Write-Host "Baue den Installer ..."
New-Item -ItemType Directory -Force dist | Out-Null
& $iscc /Q "/DAppVersion=$version" installer\vyre.iss
if ($LASTEXITCODE -ne 0) { throw "Inno Setup fehlgeschlagen" }

$setup = "vyre-setup-$version.exe"
Copy-Item "dist\$setup" dist\vyre-setup.exe -Force
$hash = (Get-FileHash "dist\$setup" -Algorithm SHA256).Hash.ToLower()
$manifest = [ordered]@{ version = $version; url = $setup; sha256 = $hash; notes = $Notes }
# UTF-8 ohne BOM (das "utf8" von Windows PowerShell würde eins schreiben).
[IO.File]::WriteAllText("$root\dist\update.json", ($manifest | ConvertTo-Json), (New-Object Text.UTF8Encoding $false))

$size = [math]::Round((Get-Item "dist\$setup").Length / 1MB, 1)
Write-Host "dist\$setup ($size MB)"
Write-Host "dist\update.json (sha256 $hash)"

if ($Publish) {
    Write-Host "Veroeffentliche v$version auf GitHub ..."
    $text = if ($Notes) { $Notes } else { "Vyre $version" }
    gh release create "v$version" "dist\$setup" dist\vyre-setup.exe dist\update.json --repo Colniro/vyre --title "Vyre $version" --notes $text
    if ($LASTEXITCODE -ne 0) { throw "gh release create fehlgeschlagen" }
}
