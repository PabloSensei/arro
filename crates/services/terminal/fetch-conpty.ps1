# Downloads the pinned ConPTY NuGet package and copies the x64 runtime into vendor/x64.
# The hashes live in conpty.lock, so the work happens in xtask.
$ErrorActionPreference = 'Stop'
cargo run -p xtask -- fetch-conpty
exit $LASTEXITCODE
