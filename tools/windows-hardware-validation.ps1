#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$EvidenceRoot,
    [switch]$PhysicalMachine,
    [string]$GamePath,
    [string]$Executable,
    [string[]]$GameArguments = @()
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'This harness requires Windows and installed WinFsp driver/SDK.' }
if ([bool]$GamePath -ne [bool]$Executable) { throw 'GamePath and Executable must be provided together.' }
$repo = Split-Path $PSScriptRoot -Parent
$work = [IO.Path]::GetFullPath($EvidenceRoot)
if (Test-Path -LiteralPath $work) { throw 'EvidenceRoot must not exist: old evidence is never overwritten.' }
New-Item -ItemType Directory -Path $work | Out-Null
Set-Location $repo
$result = [ordered]@{ status='FAIL'; windows_physical_validation='BLOCKED_ON_PHYSICAL_WINDOWS'; game_validation='NOT RUN'; commands=@(); error=$null }
try {
$system = Get-CimInstance Win32_ComputerSystem
$os = Get-CimInstance Win32_OperatingSystem
$cpu = Get-CimInstance Win32_Processor
$virtual = "$($system.Manufacturer) $($system.Model)" -match '(?i)virtual|vmware|qemu|kvm|hyper-v|parallels|virtualbox|amazon ec2|hvm|xen|bochs|bhyve'
$arch = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
$dll = switch ($arch) { 'Arm64' { 'winfsp-a64.dll' } 'X86' { 'winfsp-x86.dll' } default { 'winfsp-x64.dll' } }
$winfsp = Join-Path ${env:ProgramFiles(x86)} "WinFsp\bin\$dll"
$physicalStatus = if ($PhysicalMachine -and -not $virtual -and $os.ProductType -eq 1) { 'NOT RUN' } else { 'BLOCKED_ON_PHYSICAL_WINDOWS' }
$environment = [ordered]@{
    timestamp = [DateTime]::UtcNow.ToString('o')
    git_sha = (& git rev-parse HEAD)
    working_tree_status = (& git status --porcelain | Out-String)
    rust = (& rustc --version)
    windows = $os.Caption
    windows_build = $os.BuildNumber
    windows_product_type = $os.ProductType
    process_architecture = $arch
    architecture = $os.OSArchitecture
    cpu = @($cpu | Select-Object Name,NumberOfCores,NumberOfLogicalProcessors)
    ram_bytes = $system.TotalPhysicalMemory
    manufacturer = $system.Manufacturer
    model = $system.Model
    virtual_machine_detected = $virtual
    physical_machine_attested = [bool]$PhysicalMachine
    filesystems = @(Get-Volume | Select-Object DriveLetter,FileSystem,Size,SizeRemaining)
    winfsp_installed = (Test-Path -LiteralPath $winfsp)
    winfsp_version = if (Test-Path -LiteralPath $winfsp) { (Get-Item -LiteralPath $winfsp).VersionInfo.FileVersion } else { $null }
    windows_physical_validation = $physicalStatus
}
$environment | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $work 'environment.json') -Encoding utf8
$result.windows_physical_validation=$physicalStatus
} catch {
    $result.error=$_.Exception.Message
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $work 'result.json') -Encoding utf8
    throw
}
function Invoke-Checked([string]$Program, [string[]]$Arguments, [string]$Label) {
    $result.commands += ,(@($Program) + $Arguments)
    # Direct invocation with argument arrays: no shell command string or Invoke-Expression.
    & $Program @Arguments 1> (Join-Path $work "$Label.stdout.log") 2> (Join-Path $work "$Label.stderr.log")
    if ($LASTEXITCODE -ne 0) { throw "$Label exited $LASTEXITCODE (see preserved logs)." }
}
try {
    if (-not $environment.winfsp_installed) { throw 'Install and approve WinFsp 2.1 driver and Developer SDK first; this harness never installs drivers.' }
    Invoke-Checked 'cargo' @('build','--locked','--release','--workspace') 'build'
    Invoke-Checked 'python' @('tools/mounted-smoke.py','--work',(Join-Path $work 'readonly'),'--playsparse','target/release/playsparse.exe','--io-probe','target/release/io-probe.exe','--world-bytes','10737418240','--iterations','32','--cache','64M') 'readonly'
    Invoke-Checked 'python' @('tools/mounted-update.py','--work',(Join-Path $work 'writable'),'--playsparse','target/release/playsparse.exe','--io-probe','target/release/io-probe.exe') 'writable'
    Invoke-Checked 'python' @('tools/adaptive-smoke.py','--work',(Join-Path $work 'adaptive'),'--playsparse','target/release/playsparse.exe','--io-probe','target/release/io-probe.exe') 'adaptive'
    Invoke-Checked 'python' @('tools/tiered-smoke.py','--work',(Join-Path $work 'tiers'),'--playsparse','target/release/playsparse.exe','--io-probe','target/release/io-probe.exe') 'tiers'
    if ($GamePath) {
        Invoke-Checked 'python' (@('tools/owned-application.py','--work',(Join-Path $work 'application'),'--playsparse','target/release/playsparse.exe','--source',$GamePath,'--executable',$Executable,'--') + $GameArguments) 'application'
        $result.game_validation='PASS (one user-owned application execution; no general launcher/DRM/anti-cheat claim)'
    }
    $result.status='PASS'
    if ($PhysicalMachine -and -not $virtual -and $os.ProductType -eq 1) { $result.windows_physical_validation='PASS' }
} catch { $result.error=$_.Exception.Message; throw }
finally {
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $work 'result.json') -Encoding utf8
    Write-Host "Evidence: $work"
    Write-Host "Physical Windows validation: $($result.windows_physical_validation)"
}
