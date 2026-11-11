# Measures memory and idle CPU of a process and of its process tree over a fixed
# window. Method and rules for results are in team/reports/measurement.md.
#
#   pwsh tools/measure/measure.ps1 -Id 1234
#   pwsh tools/measure/measure.ps1 -Name ide -WindowSeconds 60 -Runs 3 -Out result.json
#   pwsh tools/measure/measure.ps1 -Start target/release/spike.exe -SettleSeconds 10
param(
    [int] $Id,
    [string] $Name,
    [string] $Start,
    [string[]] $StartArgs = @(),
    [int] $SettleSeconds = 10,
    [int] $WindowSeconds = 60,
    [int] $IntervalSeconds = 1,
    [int] $Runs = 3,
    [double] $NoisySystemPercent = 5,
    [string] $ReferencePowerPlan = '381b4222-f694-41f0-9685-ff5bb260df2e',
    [string] $Out
)

$ErrorActionPreference = 'Stop'
Add-Type -Path (Join-Path $PSScriptRoot 'Native.cs')

function Resolve-Target {
    if ($Id) { return (Get-Process -Id $Id).Id }
    $found = @(Get-Process -Name $Name)
    if ($found.Count -ne 1) {
        throw "-Name $Name matched $($found.Count) processes, pass -Id"
    }
    $found[0].Id
}

function Get-Median([double[]] $values) {
    $sorted = @($values | Sort-Object)
    if ($sorted.Count -eq 0) { return 0 }
    $middle = [int][math]::Floor($sorted.Count / 2)
    if ($sorted.Count % 2) { return $sorted[$middle] }
    ($sorted[$middle - 1] + $sorted[$middle]) / 2
}

function Get-Summary([double[]] $bytes) {
    [ordered]@{
        median_mb = [math]::Round((Get-Median $bytes) / 1MB, 1)
        max_mb = [math]::Round((($bytes | Measure-Object -Maximum).Maximum) / 1MB, 1)
    }
}

# Sums memory of a set of samples.
function Get-Sum($samples) {
    $sum = @{ ws = [double]0; pb = [double]0; total = [double]0 }
    foreach ($sample in $samples) {
        $sum.ws += $sample.PrivateWorkingSet
        $sum.pb += $sample.PrivateBytes
        $sum.total += $sample.WorkingSet
    }
    $sum
}

# Cycles and CPU time spent inside the window, from the first and the last sample of
# each process. A process born in the window counts from zero.
function Get-CpuDelta($first, $last) {
    $cycles = [double]0
    $time = [double]0
    foreach ($key in $last.Keys) {
        $baseCycles = if ($first.ContainsKey($key)) { [double]$first[$key].Cycles } else { 0 }
        $baseTime = if ($first.ContainsKey($key)) { [double]$first[$key].CpuTime100ns } else { 0 }
        $cycles += [double]$last[$key].Cycles - $baseCycles
        $time += [double]$last[$key].CpuTime100ns - $baseTime
    }
    @{ cycles = $cycles; time100ns = $time }
}

function Get-TopConsumers($before, $after, [double] $cyclesPerSecond, [double] $seconds) {
    $previous = @{}
    foreach ($sample in $before) { $previous[$sample.Key] = $sample }
    $rows = foreach ($sample in $after) {
        if ($sample.Pid -eq 0 -or -not $previous.ContainsKey($sample.Key)) { continue }
        $delta = [double]$sample.Cycles - [double]$previous[$sample.Key].Cycles
        [pscustomobject]@{
            name = $sample.Name
            pid = $sample.Pid
            core_percent = [math]::Round($delta / $cyclesPerSecond / $seconds * 100, 2)
        }
    }
    @($rows | Sort-Object core_percent -Descending | Select-Object -First 5)
}

function Invoke-Run([int] $rootPid, [double] $cyclesPerSecond) {
    $known = [Collections.Generic.HashSet[string]]::new()
    $rootSeries = [Collections.Generic.List[hashtable]]::new()
    $treeSeries = [Collections.Generic.List[hashtable]]::new()
    $firstSeen = @{}
    $lastSeen = @{}
    $allBefore = [MeasureNative]::SnapshotAll()
    $systemBefore = [MeasureNative]::SystemTimes()
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $inaccessible = [Collections.Generic.HashSet[string]]::new()
    $next = 0

    while ($true) {
        $all = [MeasureNative]::SnapshotAll()
        $tree = [MeasureNative]::SelectTree($all, $rootPid, $known)
        $root = @($tree | Where-Object { $_.Pid -eq $rootPid })
        if ($root.Count -eq 0) { throw "process $rootPid exited during the window" }
        foreach ($sample in $tree) {
            if (-not $sample.Accessible) { [void]$inaccessible.Add("$($sample.Name):$($sample.Pid)") }
            if ($next -eq 0) { $firstSeen[$sample.Key] = $sample }
            $lastSeen[$sample.Key] = $sample
        }
        $rootSeries.Add((Get-Sum $root))
        $treeSeries.Add((Get-Sum $tree))
        if ($watch.Elapsed.TotalSeconds -ge $WindowSeconds) { break }
        $next += $IntervalSeconds
        $wait = $next - $watch.Elapsed.TotalSeconds
        if ($wait -gt 0) { Start-Sleep -Milliseconds ([int]($wait * 1000)) }
    }

    $seconds = $watch.Elapsed.TotalSeconds
    $systemAfter = [MeasureNative]::SystemTimes()
    $allAfter = [MeasureNative]::SnapshotAll()
    $rootKey = @($firstSeen.Keys | Where-Object { $_.StartsWith("${rootPid}:") })[0]
    $rootCpu = Get-CpuDelta @{ $rootKey = $firstSeen[$rootKey] } @{ $rootKey = $lastSeen[$rootKey] }
    $treeCpu = Get-CpuDelta $firstSeen $lastSeen
    $alive = @{}
    foreach ($sample in $allAfter) { $alive[$sample.Key] = $true }
    $exited = @($lastSeen.Keys | Where-Object { -not $alive.ContainsKey($_) }).Count

    $busy = [double]($systemAfter[0] - $systemBefore[0])
    $total = [double]($systemAfter[1] - $systemBefore[1])
    $systemPercent = if ($total -gt 0) { $busy / $total * 100 } else { 0 }
    $logical = [Environment]::ProcessorCount
    $treeShare = $treeCpu.time100ns / ($seconds * 1e7 * $logical) * 100
    $otherPercent = [math]::Max(0, $systemPercent - $treeShare)

    [ordered]@{
        seconds = [math]::Round($seconds, 2)
        samples = $rootSeries.Count
        root = [ordered]@{
            private_ws = Get-Summary @($rootSeries | ForEach-Object { $_.ws })
            private_bytes = Get-Summary @($rootSeries | ForEach-Object { $_.pb })
            shared_ws = Get-Summary @($rootSeries | ForEach-Object { $_.total - $_.ws })
            cpu_core_percent = [math]::Round($rootCpu.cycles / $cyclesPerSecond / $seconds * 100, 3)
            cpu_time_core_percent = [math]::Round($rootCpu.time100ns / 1e7 / $seconds * 100, 3)
        }
        tree = [ordered]@{
            processes = $lastSeen.Count
            exited_in_window = $exited
            inaccessible = @($inaccessible)
            private_ws = Get-Summary @($treeSeries | ForEach-Object { $_.ws })
            private_bytes = Get-Summary @($treeSeries | ForEach-Object { $_.pb })
            cpu_core_percent = [math]::Round($treeCpu.cycles / $cyclesPerSecond / $seconds * 100, 3)
        }
        noise = [ordered]@{
            system_busy_percent = [math]::Round($systemPercent, 2)
            other_busy_percent = [math]::Round($otherPercent, 2)
            noisy = ($otherPercent -gt $NoisySystemPercent)
            top = Get-TopConsumers $allBefore $allAfter $cyclesPerSecond $seconds
        }
    }
}

function Get-RunsMedian($runs, [scriptblock] $pick) {
    [math]::Round((Get-Median @($runs | ForEach-Object { [double](& $pick $_) })), 3)
}

$started = $null
if ($Start) {
    $started = Start-Process -FilePath $Start -ArgumentList $StartArgs -PassThru
    $targetId = $started.Id
    Start-Sleep -Seconds $SettleSeconds
} elseif ($Id -or $Name) {
    $targetId = Resolve-Target
    Start-Sleep -Seconds $SettleSeconds
} else {
    throw 'Pass -Id, -Name or -Start'
}

try {
    $target = Get-Process -Id $targetId
    $conditions = & (Join-Path $PSScriptRoot 'conditions.ps1') -AsObject
    $cyclesPerSecond = [MeasureNative]::CalibrateCyclesPerSecond()
    $runResults = @(1..$Runs | ForEach-Object { Invoke-Run $targetId $cyclesPerSecond })
} finally {
    if ($started -and -not $started.HasExited) { taskkill /PID $started.Id /T /F | Out-Null }
}

$noisyRuns = @($runResults | Where-Object { $_.noise.noisy }).Count
$planMatches = $conditions.power_plan.guid -eq $ReferencePowerPlan
$report = [ordered]@{
    schema = 1
    target = [ordered]@{ pid = $targetId; name = $target.ProcessName; path = $target.Path }
    settle_seconds = $SettleSeconds
    window_seconds = $WindowSeconds
    interval_seconds = $IntervalSeconds
    cycles_per_second = [math]::Round($cyclesPerSecond)
    conditions = $conditions
    result = [ordered]@{
        cpu_valid = ($noisyRuns -eq 0 -and $planMatches -and -not $conditions.on_battery)
        noisy_runs = $noisyRuns
        power_plan_matches = $planMatches
        root_private_ws_mb = Get-RunsMedian $runResults { param($r) $r.root.private_ws.median_mb }
        root_private_bytes_mb = Get-RunsMedian $runResults { param($r) $r.root.private_bytes.median_mb }
        root_cpu_core_percent = Get-RunsMedian $runResults { param($r) $r.root.cpu_core_percent }
        tree_private_ws_mb = Get-RunsMedian $runResults { param($r) $r.tree.private_ws.median_mb }
        tree_private_bytes_mb = Get-RunsMedian $runResults { param($r) $r.tree.private_bytes.median_mb }
        tree_cpu_core_percent = Get-RunsMedian $runResults { param($r) $r.tree.cpu_core_percent }
    }
    runs = $runResults
}

$json = $report | ConvertTo-Json -Depth 8
if ($Out) {
    Set-Content -Path $Out -Value $json -Encoding utf8
}
$r = $report.result
"target {0} pid {1}, {2} runs of {3} s, cpu_valid: {4}" -f $target.ProcessName, $targetId, $Runs, $WindowSeconds, $r.cpu_valid
"root  Private WS {0} MB, Private Bytes {1} MB, CPU {2}% of a core" -f $r.root_private_ws_mb, $r.root_private_bytes_mb, $r.root_cpu_core_percent
"tree  Private WS {0} MB, Private Bytes {1} MB, CPU {2}% of a core" -f $r.tree_private_ws_mb, $r.tree_private_bytes_mb, $r.tree_cpu_core_percent
if (-not $Out) { $json }
