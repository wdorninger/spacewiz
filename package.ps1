# Builds the portable release zip: dist\spacewiz-<version>-windows-x64-portable.zip
#
# Local paths (user profile, cargo registry, this folder) are replaced by generic ones in the executable,
# so the published binary does not reveal where or by whom it was built. The build goes to target\dist to
# keep the normal build cache.

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
$repo = (Get-Location).Path
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$rustup = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
$targetDir = Join-Path $repo 'target\dist'

# RUSTFLAGS replaces the rustflags of .cargo\config.toml, so the static C runtime flag is repeated here.
$flags = @(
    '-C', 'target-feature=+crt-static',
    "--remap-path-prefix=$cargoHome=/cargo",
    "--remap-path-prefix=$rustup=/rustup",
    "--remap-path-prefix=$targetDir=/target",
    "--remap-path-prefix=$repo=/spacewiz",
    "--remap-path-prefix=$($env:USERPROFILE)=/home"
)
$env:RUSTFLAGS = $flags -join ' '
cargo build --release --target-dir $targetDir
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
Remove-Item Env:RUSTFLAGS

$exe = Join-Path $targetDir 'release\spacewiz.exe'

# Refuse to package if any local path is still inside the binary.
$text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($exe))
$unicode = [Text.Encoding]::Unicode.GetString([IO.File]::ReadAllBytes($exe))
$leaks = foreach ($needle in @($env:USERNAME, $env:USERPROFILE, $repo, 'C:\Users\')) {
    foreach ($s in @($text, $unicode)) {
        $n = ([regex]::Matches($s, [regex]::Escape($needle), 'IgnoreCase')).Count
        if ($n -gt 0) { "$needle ($n)" }
    }
}
if ($leaks) { throw "Local paths found in the executable: $($leaks -join ', ')" }

$name = "spacewiz-$version-windows-x64"
$dir = Join-Path 'dist' $name
$zip = Join-Path 'dist' "$name-portable.zip"
if (Test-Path dist) { Remove-Item dist -Recurse -Force }
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item $exe, README.md $dir
Compress-Archive -Path $dir -DestinationPath $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash
Write-Host "Created $zip"
Write-Host "SHA-256 $hash"
