#Requires -Version 7.0
# Host discovery + argument-safe handoff. Python owns lifecycle and evidence.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$EvidenceRoot,
    [switch]$PhysicalMachine,
    [string]$GamePath,
    [string]$Executable,
    [string[]]$GameArguments = @(),
    [switch]$WofComparison,
    [string]$WofSource,
    [ValidateSet('XPRESS4K','XPRESS8K','XPRESS16K','LZX')][string]$WofAlgorithm = 'XPRESS4K',
    [ValidateRange(60,86400)][int]$StageTimeoutSeconds = 1800,
    [switch]$AllowDirty,
    [string]$Python = 'python'
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$environment = [ordered]@{
    timestamp = [DateTime]::UtcNow.ToString('o')
    is_windows = [bool]$IsWindows
    platform = [Runtime.InteropServices.RuntimeInformation]::OSDescription
    process_architecture = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
    architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    powershell = $PSVersionTable.PSVersion.ToString()
    virtual_machine_detected = $null
    physical_machine_attested = [bool]$PhysicalMachine
}
try {
    if ($IsWindows) {
        $system = Get-CimInstance Win32_ComputerSystem
        $os = Get-CimInstance Win32_OperatingSystem
        $cpu = Get-CimInstance Win32_Processor
        $bios = Get-CimInstance Win32_BIOS
        $hintText = "$($system.Manufacturer) $($system.Model) $($bios.Manufacturer) $($bios.Version)"
        $environment.windows = $os.Caption
        $environment.windows_version = $os.Version
        $environment.windows_build = $os.BuildNumber
        $environment.windows_product_type = [int]$os.ProductType
        $environment.cpu = @($cpu | Select-Object Name,NumberOfCores,NumberOfLogicalProcessors)
        $environment.ram_bytes = $system.TotalPhysicalMemory
        $environment.manufacturer = $system.Manufacturer
        $environment.model = $system.Model
        $environment.virtual_machine_detected = [bool]($hintText -match '(?i)virtual|vmware|qemu|kvm|hyper-v|parallels|virtualbox|amazon ec2|hvm|xen|bochs|bhyve')
        $environment.virtualization_basis = 'CIM manufacturer/model/BIOS hints; absence is not proof of a physical machine'
        $environment.filesystems = @(Get-Volume | Select-Object DriveLetter,FileSystem,FileSystemLabel,Size,SizeRemaining)
        $environment.disks = @(Get-Disk | Select-Object Number,FriendlyName,BusType,PartitionStyle,Size,IsBoot,IsSystem)
        $installRoots = @()
        foreach ($key in @('HKLM:\SOFTWARE\WinFsp','HKLM:\SOFTWARE\WOW6432Node\WinFsp')) {
            if (Test-Path -LiteralPath $key) { $installRoots += (Get-ItemProperty -LiteralPath $key).InstallDir }
        }
        $programFiles86 = [Environment]::GetEnvironmentVariable('ProgramFiles(x86)')
        if ($programFiles86) { $installRoots += (Join-Path $programFiles86 'WinFsp') }
        if ($env:ProgramFiles) { $installRoots += (Join-Path $env:ProgramFiles 'WinFsp') }
        $environment.winfsp = @(
            foreach ($root in @($installRoots | Where-Object { $_ } | Select-Object -Unique)) {
                foreach ($dll in @('winfsp-x64.dll','winfsp-a64.dll','winfsp-x86.dll')) {
                    $candidate = Join-Path $root "bin\$dll"
                    if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                        $item = Get-Item -LiteralPath $candidate
                        [ordered]@{ path=$item.FullName; version=$item.VersionInfo.FileVersion;
                            sdk_headers_present=(Test-Path -LiteralPath (Join-Path $root 'inc\winfsp\winfsp.h')) }
                    }
                }
            }
        )
    }
} catch { $environment.preflight_error = $_.Exception.Message }

# ArgumentList performs native argument escaping on every PS7 version.
# No Start-Process joined string, shell or Invoke-Expression is used.
$start = [Diagnostics.ProcessStartInfo]::new()
$start.FileName = $Python
$start.WorkingDirectory = $repo
$start.UseShellExecute = $false
$start.RedirectStandardInput = $true
$start.StandardInputEncoding = [Text.UTF8Encoding]::new($false)
$start.Environment['PYTHONUTF8'] = '1'
$start.ArgumentList.Add((Join-Path $PSScriptRoot 'windows-validation.py'))
$start.ArgumentList.Add('--work'); $start.ArgumentList.Add($EvidenceRoot)
$start.ArgumentList.Add('--environment-stdin')
$start.ArgumentList.Add('--timeout'); $start.ArgumentList.Add($StageTimeoutSeconds.ToString())
if ($PhysicalMachine) { $start.ArgumentList.Add('--physical-machine') }
if ($AllowDirty) { $start.ArgumentList.Add('--allow-dirty') }
if ($WofComparison) { $start.ArgumentList.Add('--wof-comparison') }
if ($WofSource) { $start.ArgumentList.Add('--wof-source'); $start.ArgumentList.Add($WofSource) }
$start.ArgumentList.Add('--wof-algorithm'); $start.ArgumentList.Add($WofAlgorithm)
if ($GamePath) { $start.ArgumentList.Add('--game-path'); $start.ArgumentList.Add($GamePath) }
if ($Executable) { $start.ArgumentList.Add('--executable'); $start.ArgumentList.Add($Executable) }
if ($GameArguments.Count) {
    $start.ArgumentList.Add('--')
    foreach ($argument in $GameArguments) { $start.ArgumentList.Add($argument) }
}
$process = [Diagnostics.Process]::new()
$process.StartInfo = $start
$started = $false
$code = 2
try {
    $started = $process.Start()
    if (-not $started) { throw 'Could not start Python validation runner.' }
    $process.StandardInput.Write(($environment | ConvertTo-Json -Depth 12 -Compress))
    $process.StandardInput.Close()
    $process.WaitForExit()
    $code = $process.ExitCode
} catch {
    [Console]::Error.WriteLine("Validation runner could not complete: $($_.Exception.Message). Python 3.9+ is required. Existing evidence is preserved.")
} finally {
    if ($started -and -not $process.HasExited) {
        # Allow Python's signal handler to publish failure and ordinary cleanup.
        if (-not $process.WaitForExit(30000)) {
            $process.Kill($true)
            $process.WaitForExit(30000) | Out-Null
        }
    }
    $process.Dispose()
}
exit $code
