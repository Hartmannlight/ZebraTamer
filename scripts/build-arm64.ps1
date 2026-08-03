[CmdletBinding()]
param(
    [string]$OutputDirectory = "dist/arm64-local"
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$outputPath = [System.IO.Path]::GetFullPath((Join-Path $projectRoot $OutputDirectory))
$target = "aarch64-unknown-linux-gnu"
$crossImage = "zpl-agent-cross:aarch64-rust-1-bookworm"

New-Item -ItemType Directory -Force -Path $outputPath | Out-Null

Push-Location $projectRoot
try {
    $commit = (git rev-parse HEAD).Trim()
    Write-Host "Preparing native x86_64 -> ARM64 compiler image..."
    docker build --file Dockerfile.cross-aarch64 --tag $crossImage .
    if ($LASTEXITCODE -ne 0) { throw "Unable to prepare cross-compiler image" }

    Write-Host "Building zpl-agent for $target (using persistent cache)..."
    docker run --rm `
        --mount "type=bind,source=$projectRoot,target=/work" `
        --mount "type=bind,source=$outputPath,target=/out" `
        --mount "type=volume,source=zpl-agent-cargo-registry,target=/usr/local/cargo/registry" `
        --mount "type=volume,source=zpl-agent-cargo-git,target=/usr/local/cargo/git" `
        --mount "type=volume,source=zpl-agent-arm64-target,target=/target" `
        --workdir /work `
        --env "CARGO_TARGET_DIR=/target" `
        --env "ZPL_AGENT_GIT_COMMIT=$commit" `
        $crossImage `
        bash -c "cargo build --locked --release --target $target && cp /target/$target/release/zpl-agent /out/zpl-agent"
    if ($LASTEXITCODE -ne 0) { throw "ARM64 build failed" }
}
finally {
    Pop-Location
}

$binary = Join-Path $outputPath "zpl-agent"
if (-not (Test-Path $binary)) { throw "Build completed without producing $binary" }
$hash = (Get-FileHash $binary -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  zpl-agent" | Set-Content -NoNewline (Join-Path $outputPath "zpl-agent.sha256")
Write-Host "Built $binary"
Write-Host "SHA-256: $hash"
