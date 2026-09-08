[CmdletBinding()]
param(
    [string]$OutputDirectory = "dist/aarch64-local"
)

$ErrorActionPreference = "Stop"
$commit = (git rev-parse HEAD).Trim()

docker buildx build `
    --platform linux/arm64 `
    --target artifact `
    --build-arg "ZPL_AGENT_GIT_COMMIT=$commit" `
    --build-arg "CARGO_PROFILE_RELEASE_LTO=false" `
    --build-arg "CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16" `
    --output "type=local,dest=$OutputDirectory" `
    .

$binary = Join-Path $OutputDirectory "zpl-agent"
$hash = (Get-FileHash $binary -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  zpl-agent" | Set-Content -NoNewline (Join-Path $OutputDirectory "zpl-agent.sha256")

Write-Host "Built $binary"
Write-Host "SHA-256: $hash"
