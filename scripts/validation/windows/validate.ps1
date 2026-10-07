param([Parameter(Mandatory=$true)][string]$App, [Parameter(Mandatory=$true)][string]$Work)
$ErrorActionPreference = 'Stop'
$kit = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$manifest = Get-Content (Join-Path $kit 'build-manifest.json') -Raw | ConvertFrom-Json
foreach ($entry in $manifest.sha256.PSObject.Properties) {
  if ([IO.Path]::GetFileName($entry.Name) -ne $entry.Name) { throw 'Invalid manifest path' }
  if ((Get-FileHash (Join-Path $kit $entry.Name) -Algorithm SHA256).Hash.ToLowerInvariant() -ne $entry.Value) { throw "Kit checksum mismatch: $($entry.Name)" }
}
& (Join-Path $kit 'playsparse-validation.exe') --app $App --work $Work --engine (Join-Path $kit 'playsparse-engine.exe') --fixture (Join-Path $kit 'playsparse-fixture.exe') --commit $manifest.commit_SHA --evidence 'physical hardware validation'
if ($LASTEXITCODE -ne 0) { throw 'Validation failed; retain work directory, receipt and live sessions. No force-unmount is performed.' }
