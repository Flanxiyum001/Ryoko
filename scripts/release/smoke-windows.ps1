$ErrorActionPreference = 'Stop'
$report = Join-Path $env:RUNNER_TEMP 'ryoko-smoke-report.txt'
@("source_sha=$env:SOURCE_SHA", "source_run=$env:SOURCE_RUN", 'platform=windows-x64') | Set-Content $report
$files = @(Get-ChildItem (Join-Path $env:RUNNER_TEMP 'ryoko-smoke-input') -Recurse -File -Filter '*.msi')
if ($files.Count -ne 1) { throw 'Expected exactly one MSI' }
$installer = $files[0].FullName
Add-Content $report "installer_sha256=$((Get-FileHash $installer -Algorithm SHA256).Hash.ToLowerInvariant())"
Add-Content $report "authenticode=$((Get-AuthenticodeSignature $installer).Status)"
$roots = @((Join-Path $env:ProgramFiles 'Ryoko'), (Join-Path $env:LOCALAPPDATA 'Ryoko'), (Join-Path $env:LOCALAPPDATA 'Programs\Ryoko'))
if (@($roots | Where-Object { Test-Path $_ }).Count -ne 0) { throw 'Ryoko already present; use a disposable runner' }
$installed = $false
$app = $null
try {
  $install = Start-Process msiexec.exe -ArgumentList @('/i', ('"' + $installer + '"'), '/qn', '/norestart') -Wait -PassThru
  if ($install.ExitCode -notin @(0,3010)) { throw "MSI installation failed: $($install.ExitCode)" }
  $installed = $true
  Add-Content $report 'install=passed'
  $candidates = @($roots | Where-Object { Test-Path $_ } | ForEach-Object {
    Get-ChildItem $_ -Recurse -File -ErrorAction Stop | Where-Object { $_.Name -in @('ryoko-desktop.exe', 'Ryoko.exe') }
  })
  if ($candidates.Count -ne 1) { throw 'Expected exactly one installed desktop executable' }
  $app = Start-Process -FilePath $candidates[0].FullName -PassThru
  Start-Sleep -Seconds 15
  $app.Refresh()
  if ($app.HasExited) { throw "Desktop process exited: $($app.ExitCode)" }
  Add-Content $report 'launch=process-survived-15s (not UI verification)'
} finally {
  if ($app -and -not $app.HasExited) { Stop-Process -Id $app.Id -Force; $app.WaitForExit() }
  if ($installed) {
    $remove = Start-Process msiexec.exe -ArgumentList @('/x', ('"' + $installer + '"'), '/qn', '/norestart') -Wait -PassThru
    if ($remove.ExitCode -notin @(0,3010)) { Add-Content $report 'uninstall=failed'; throw "MSI uninstall failed: $($remove.ExitCode)" }
    if ($candidates -and (Test-Path $candidates[0].FullName)) { throw 'Desktop executable still exists after uninstall' }
    Add-Content $report 'uninstall=passed'
  }
  Add-Content $report @('workspace_edit_save_relaunch=manual-required','nsis=manual-required','secret_scan=not-run')
}
