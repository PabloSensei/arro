# Records the conditions a measurement ran under. It only reads: the power plan and
# running programs are never changed from here. Prints JSON, or returns an object
# when called with -AsObject.
param([switch] $AsObject)

$ErrorActionPreference = 'Stop'

# Programs that make a memory or CPU run unrepresentative.
$NoisyNames = @(
    'java', 'javac', 'cargo', 'rustc', 'link', 'cl', 'clang', 'node', 'idea64',
    'msbuild', 'devenv', 'ollama', 'steam', 'obs64', 'vmmem', 'vmmemwsl'
)

function Get-PowerPlan {
    $line = powercfg /getactivescheme
    if ($line -match '([0-9a-fA-F-]{36})\s+\((.+)\)') {
        return [ordered]@{ guid = $Matches[1]; name = $Matches[2] }
    }
    [ordered]@{ guid = $null; name = "$line" }
}

function Get-Conditions {
    $os = Get-CimInstance Win32_OperatingSystem
    $cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
    $battery = Get-CimInstance Win32_Battery | Select-Object -First 1
    $gpus = Get-CimInstance Win32_VideoController | ForEach-Object {
        [ordered]@{
            name = $_.Name
            driver = $_.DriverVersion
            width = $_.CurrentHorizontalResolution
            height = $_.CurrentVerticalResolution
            refresh_hz = $_.CurrentRefreshRate
        }
    }
    $disks = Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3' | ForEach-Object {
        [ordered]@{ drive = $_.DeviceID; free_gb = [math]::Round($_.FreeSpace / 1GB, 1) }
    }
    $processes = Get-Process
    $noisy = $processes | Where-Object { $NoisyNames -contains $_.ProcessName } |
        Group-Object ProcessName | ForEach-Object {
            [ordered]@{ name = $_.Name; count = $_.Count }
        }
    $topMemory = $processes | Sort-Object WorkingSet64 -Descending | Select-Object -First 8 |
        ForEach-Object {
            [ordered]@{ name = $_.ProcessName; pid = $_.Id; ws_mb = [int]($_.WorkingSet64 / 1MB) }
        }
    $defender = $null
    try {
        $defender = (Get-MpComputerStatus).RealTimeProtectionEnabled
    } catch {
        $defender = 'unknown'
    }
    [ordered]@{
        taken_at = (Get-Date).ToString('o')
        host = $env:COMPUTERNAME
        os = "$($os.Caption) $($os.Version)"
        last_boot = $os.LastBootUpTime.ToString('o')
        cpu = [ordered]@{
            name = $cpu.Name.Trim()
            cores = $cpu.NumberOfCores
            logical = $cpu.NumberOfLogicalProcessors
        }
        ram_gb = [math]::Round($os.TotalVisibleMemorySize / 1MB, 1)
        ram_free_gb = [math]::Round($os.FreePhysicalMemory / 1MB, 1)
        power_plan = Get-PowerPlan
        on_battery = [bool]($battery -and $battery.BatteryStatus -eq 1)
        gpus = @($gpus)
        disks = @($disks)
        defender_realtime = $defender
        process_count = $processes.Count
        noisy_processes = @($noisy)
        top_memory = @($topMemory)
    }
}

$conditions = Get-Conditions
if ($AsObject) { return $conditions }
$conditions | ConvertTo-Json -Depth 5
