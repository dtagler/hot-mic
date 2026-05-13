$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

if (-not (docker image inspect hotmic-builder 2>$null)) {
    Write-Host "Building hotmic-builder image..."
    docker build -t hotmic-builder $here
}

docker run --rm -v "${here}:/work" hotmic-builder `
    cargo build --release --target x86_64-pc-windows-gnu

$exe = Join-Path $here 'target\x86_64-pc-windows-gnu\release\hotmic.exe'
if (-not (Test-Path $exe)) {
    Write-Error "Build did not produce $exe"
}

$dist = Join-Path $here 'dist'
if (-not (Test-Path $dist)) { New-Item -ItemType Directory -Path $dist | Out-Null }
Copy-Item -Force $exe (Join-Path $dist 'hotmic.exe')

$size = (Get-Item (Join-Path $dist 'hotmic.exe')).Length
Write-Host ("Built: dist\hotmic.exe ({0:N0} bytes)" -f $size)
