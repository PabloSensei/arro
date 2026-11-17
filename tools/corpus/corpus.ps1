# Manages the reference projects in corpus/: sync pinned commits, build each one
# with its own wrapper, print a table of the results.
#
#   pwsh tools/corpus/corpus.ps1 sync
#   pwsh tools/corpus/corpus.ps1 build [name...]
#   pwsh tools/corpus/corpus.ps1 clean [name...]
#   pwsh tools/corpus/corpus.ps1 status
#
# To update a project, change its Commit here and run sync. Dependency caches
# stay on E: because C: is nearly full.
param(
    [Parameter(Mandatory, Position = 0)]
    [ValidateSet('sync', 'build', 'clean', 'status')]
    [string] $Command,
    [Parameter(Position = 1, ValueFromRemainingArguments)]
    [string[]] $Name
)

$ErrorActionPreference = 'Stop'
$Corpus = if ($env:IDE_CORPUS) { $env:IDE_CORPUS } else { Join-Path $PSScriptRoot '../../corpus' }
$Cache = if ($env:IDE_CACHE) { $env:IDE_CACHE } else { 'E:/java-ide-cache' }
$MavenRepo = "-Dmaven.repo.local=$Cache/m2/repository"
$MavenBuild = @('-B', '-ntp', $MavenRepo, '-DskipTests', 'package')
$MavenClean = @('-B', '-ntp', $MavenRepo, 'clean')

$Projects = @(
    @{
        Name = 'petclinic'; Jdk = '17'; Branch = 'main'
        Url = 'https://github.com/spring-projects/spring-petclinic.git'
        Commit = '500158f732419217507c7656904b8e6aa1bcc0d6'
        Wrapper = 'mvnw.cmd'; Build = $MavenBuild; Clean = $MavenClean
    }
    @{
        Name = 'apollo'; Jdk = '17'; Branch = 'master'
        Url = 'https://github.com/apolloconfig/apollo.git'
        Commit = '0008c348ee3634fdab1c0bebcb6c475b44bd1d56'
        Wrapper = 'mvnw.cmd'; Build = $MavenBuild; Clean = $MavenClean
    }
    @{
        Name = 'guava'; Jdk = '17'; Branch = 'master'
        Url = 'https://github.com/google/guava.git'
        Commit = '688f625b7fed5f6ca39c3bfaba111238c7a481a5'
        Wrapper = 'mvnw.cmd'; Build = $MavenBuild; Clean = $MavenClean
    }
    @{
        Name = 'dgs-framework'; Jdk = '17'; Branch = 'master'
        Url = 'https://github.com/Netflix/dgs-framework.git'
        Commit = '25a217352ea7eb6de794e2493592820d5392728e'
        # The project publishes a build scan to gradle.com unless told not to.
        Wrapper = 'gradlew.bat'; Build = @('--no-daemon', '--no-scan', 'assemble', 'testClasses')
        Clean = @('--no-daemon', '--no-scan', 'clean')
    }
)

function Select-Projects {
    if (-not $Name) { return $Projects }
    $Projects | Where-Object { $Name -contains $_.Name }
}

function Invoke-Sync($p) {
    $dir = "$Corpus/src/$($p.Name)"
    if (-not (Test-Path $dir)) {
        git init --quiet $dir
        git -C $dir config core.autocrlf false
        git -C $dir config core.longpaths true
        git -C $dir remote add origin $p.Url
    }
    git -C $dir fetch --quiet --depth 1 origin $p.Commit
    git -C $dir checkout --quiet --detach $p.Commit
    if ($LASTEXITCODE) { throw "sync failed: $($p.Name)" }
    "$($p.Name) $(git -C $dir rev-parse --short HEAD)"
}

function Invoke-Wrapper($p, [string[]] $wrapperArgs, [string] $log) {
    $env:JAVA_HOME = "$Corpus/jdk/$($p.Jdk)"
    $env:MAVEN_USER_HOME = "$Cache/m2"
    $env:GRADLE_USER_HOME = "$Cache/gradle"
    New-Item -ItemType Directory -Force "$Corpus/logs" | Out-Null
    Push-Location "$Corpus/src/$($p.Name)"
    try {
        $watch = [Diagnostics.Stopwatch]::StartNew()
        # The explicit path matters: with NoDefaultCurrentDirectoryInExePath set, as in
        # Claude Code sessions, cmd does not look in the current directory.
        & cmd /c ".\$($p.Wrapper)" @wrapperArgs *> "$Corpus/logs/$log"
        "$($p.Name): exit $LASTEXITCODE, $([int]$watch.Elapsed.TotalSeconds) s, log logs/$log"
    } finally {
        Pop-Location
    }
}

function Get-Row($p) {
    $dir = "$Corpus/src/$($p.Name)"
    $size = (Get-ChildItem $dir -Recurse -File -Force | Measure-Object Length -Sum).Sum
    [pscustomobject]@{
        Name   = $p.Name
        Commit = (git -C $dir rev-parse --short=12 HEAD)
        MB     = [int]($size / 1MB)
        Files  = (git -C $dir ls-files | Measure-Object).Count
        Dirty  = (git -C $dir status --porcelain | Measure-Object).Count
    }
}

switch ($Command) {
    'sync' { Select-Projects | ForEach-Object { Invoke-Sync $_ } }
    'build' { Select-Projects | ForEach-Object { Invoke-Wrapper $_ $_.Build "$($_.Name)-build.log" } }
    'clean' { Select-Projects | ForEach-Object { Invoke-Wrapper $_ $_.Clean "$($_.Name)-clean.log" } }
    'status' { Select-Projects | ForEach-Object { Get-Row $_ } | Format-Table -AutoSize }
}
