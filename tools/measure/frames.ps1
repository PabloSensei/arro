# Computes frame times and input latency from a marks file, and frame times from a
# PresentMon CSV for the cross-check.
#
#   pwsh tools/measure/frames.ps1 -Marks marks.jsonl
#   pwsh tools/measure/frames.ps1 -PresentMon presentmon.csv -Application spike.exe
#
# A marks file is JSON Lines written by the program under test. The first line gives
# the QueryPerformanceCounter frequency, the others are events in QPC ticks:
#   {"qpc_freq":10000000}
#   {"e":"input","t":123456}     WM_KEYDOWN reached the window procedure
#   {"e":"present","t":123999}   Present returned
param(
    [string] $Marks,
    [string] $PresentMon,
    [string] $Application,
    [double] $FrameBudgetMs = 16.6,
    [string] $Out
)

$ErrorActionPreference = 'Stop'

function Get-Percentile([double[]] $sorted, [double] $percent) {
    $rank = [int][math]::Ceiling($percent / 100 * $sorted.Count) - 1
    $sorted[[math]::Max(0, $rank)]
}

function Get-Stats([double[]] $values) {
    if ($values.Count -eq 0) { return [ordered]@{ count = 0 } }
    $sorted = @($values | Sort-Object)
    [ordered]@{
        count = $sorted.Count
        p50_ms = [math]::Round((Get-Percentile $sorted 50), 3)
        p95_ms = [math]::Round((Get-Percentile $sorted 95), 3)
        p99_ms = [math]::Round((Get-Percentile $sorted 99), 3)
        max_ms = [math]::Round($sorted[-1], 3)
    }
}

function Get-FrameStats([double[]] $frames) {
    $stats = Get-Stats $frames
    $over = @($frames | Where-Object { $_ -gt $FrameBudgetMs }).Count
    $stats['budget_ms'] = $FrameBudgetMs
    $stats['over_budget'] = $over
    $stats['over_budget_percent'] = if ($frames.Count) { [math]::Round($over / $frames.Count * 100, 3) } else { 0 }
    $stats
}

function Read-Marks([string] $path) {
    $lines = Get-Content -Path $path
    $header = $lines[0] | ConvertFrom-Json
    if (-not $header.qpc_freq) { throw 'first line must be {"qpc_freq":N}' }
    $msPerTick = 1000.0 / $header.qpc_freq
    $events = @($lines | Select-Object -Skip 1 | Where-Object { $_.Trim() } |
        ForEach-Object { $_ | ConvertFrom-Json } | Sort-Object { [long]$_.t })

    $presents = [Collections.Generic.List[long]]::new()
    foreach ($event in $events) {
        if ($event.e -eq 'present') { $presents.Add([long]$event.t) }
    }
    $frames = [Collections.Generic.List[double]]::new()
    for ($i = 1; $i -lt $presents.Count; $i++) {
        $frames.Add(($presents[$i] - $presents[$i - 1]) * $msPerTick)
    }

    # Each input is matched with the first Present after it. Inputs are in time order,
    # so the search moves forward only.
    $latency = [Collections.Generic.List[double]]::new()
    $unmatched = 0
    $next = 0
    foreach ($event in $events) {
        if ($event.e -ne 'input') { continue }
        while ($next -lt $presents.Count -and $presents[$next] -le [long]$event.t) { $next++ }
        if ($next -lt $presents.Count) {
            $latency.Add(($presents[$next] - [long]$event.t) * $msPerTick)
        } else {
            $unmatched++
        }
    }
    [ordered]@{
        source = 'marks'
        frames = Get-FrameStats $frames.ToArray()
        input_latency = Get-Stats $latency.ToArray()
        inputs_without_present = $unmatched
    }
}

function Read-PresentMon([string] $path) {
    $rows = Import-Csv -Path $path
    if ($Application) { $rows = @($rows | Where-Object { $_.Application -eq $Application }) }
    if ($rows.Count -eq 0) { throw "no rows in $path" }
    # PresentMon 1.x writes msBetweenPresents, later versions write FrameTime.
    $column = @('msBetweenPresents', 'FrameTime' | Where-Object { $rows[0].PSObject.Properties[$_] })[0]
    if (-not $column) { throw 'neither msBetweenPresents nor FrameTime found' }
    $frames = @($rows | ForEach-Object { [double]::Parse($_.$column, [Globalization.CultureInfo]::InvariantCulture) })
    [ordered]@{
        source = "presentmon:$column"
        frames = Get-FrameStats $frames
    }
}

if ($Marks) {
    $report = Read-Marks $Marks
} elseif ($PresentMon) {
    $report = Read-PresentMon $PresentMon
} else {
    throw 'Pass -Marks or -PresentMon'
}

$json = $report | ConvertTo-Json -Depth 5
if ($Out) { Set-Content -Path $Out -Value $json -Encoding utf8 }
$json
