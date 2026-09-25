param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $AppArguments
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location $projectRoot
try {
    cargo build --bin jev-game-engine
    if ($LASTEXITCODE -ne 0) { throw 'Preview build failed.' }

    # A unique copy lets Cargo rebuild while this preview remains open.
    $previewDirectory = Join-Path $projectRoot ('artifacts\preview\' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $previewDirectory | Out-Null
    $previewExecutable = Join-Path $previewDirectory 'jev-game-engine.exe'
    Copy-Item -LiteralPath (Join-Path $projectRoot 'target\debug\jev-game-engine.exe') -Destination $previewExecutable
    & $previewExecutable @AppArguments
    if ($LASTEXITCODE -ne 0) { throw "Preview exited with code $LASTEXITCODE." }
} finally {
    Pop-Location
}
