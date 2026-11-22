# Remove untracked crates that cause cyclic dependencies
$dirs = @("crates\ryoko-index", "crates\ryoko-tools")
foreach ($dir in $dirs) {
    if (Test-Path $dir) {
        Remove-Item -Recurse -Force $dir
        Write-Host "Removed $dir"
    }
}

# Remove tools.rs if it exists
$tools = "crates\ryoko-core\src\tools.rs"
if (Test-Path $tools) {
    Remove-Item -Force $tools
    Write-Host "Removed $tools"
}