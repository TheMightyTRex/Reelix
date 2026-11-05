# Helpers for the Windows installer smoke tests in .github/workflows/release.yml (dot-source it
# from the workspace root after the "build installer" step).

$Setup = "installer\reelix-$env:VERSION-windows-x86_64-setup.exe"
# The portable reelix.exe from the release zip the installer was built from.
$Portable = "$env:RUNNER_TEMP\win\reelix-$env:VERSION-windows-x86_64\reelix.exe"
$InstallDir = "$env:LOCALAPPDATA\Programs\Reelix"
$ReelixData = "$env:LOCALAPPDATA\Reelix"
$Password = 'correct-horse-battery'
$UninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{6B0E2D4A-3C71-4F8B-9A52-7E1D0C4B8F23}_is1'

function Wait-Healthy([int]$Port) {
  foreach ($i in 1..30) {
    try { Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$Port/api/v1/system/health" | Out-Null; return } catch { Start-Sleep 1 }
  }
  throw "Reelix on port $Port did not become healthy"
}

# Runs the portable reelix.exe once on a new data folder, creates an admin account and stops it.
function New-ReelixData([string]$Dir, [string]$User, [int]$Port) {
  $rx = Start-Process $Portable -PassThru -ArgumentList '--port', $Port, '--data-dir', $Dir
  try {
    Wait-Healthy $Port
    Invoke-RestMethod -Method Post "http://127.0.0.1:$Port/api/v1/setup" -ContentType 'application/json' `
      -Body (@{ username = $User; password = $Password } | ConvertTo-Json) | Out-Null
  } finally {
    Stop-Process -Id $rx.Id -Force -ErrorAction SilentlyContinue
    $rx.WaitForExit()
  }
}

# Silent per-user install with extra Setup parameters; returns Setup's exit code.
function Install-Reelix([string]$Name, [string[]]$Params) {
  if (Test-Path $UninstallKey) { throw 'Reelix is still installed; the install would be an upgrade' }
  $log = "$env:RUNNER_TEMP\setup-$Name.log"
  $all = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/CURRENTUSER', '/NORESTART', "/LOG=$log") + $Params
  $p = Start-Process $Setup -Wait -PassThru -ArgumentList $all
  if ($p.ExitCode -ne 0) { Get-Content $log -ErrorAction SilentlyContinue | Select-Object -Last 40 | Write-Host }
  return $p.ExitCode
}

# Starts the installed reelix.exe on the per-user data folder and waits until it is healthy.
function Start-Installed([int]$Port) {
  $rx = Start-Process "$InstallDir\reelix.exe" -WorkingDirectory $InstallDir -PassThru `
    -ArgumentList '--port', $Port, '--data-dir', $ReelixData
  Wait-Healthy $Port
  return $rx
}

function Uninstall-Reelix {
  $p = Start-Process "$InstallDir\unins000.exe" -Wait -PassThru -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART'
  if ($p.ExitCode -ne 0) { throw "uninstall exited $($p.ExitCode)" }
  # The uninstaller re-launches itself from %TEMP%; wait for it to finish.
  foreach ($i in 1..60) {
    if (-not (Test-Path "$InstallDir\reelix.exe") -and -not (Test-Path $UninstallKey)) { return }
    Start-Sleep 1
  }
  throw 'uninstall did not finish'
}
