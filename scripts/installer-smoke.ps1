<#
.SYNOPSIS
  NSIS installer smoke test: install -> run -> (upgrade) -> uninstall, silently.

.DESCRIPTION
  MUST ONLY run on a disposable machine (GitHub Actions windows-latest runner, elevated).
  The installer registers the FerrumIndexer Windows service, so the script refuses to run
  unless CI=true / GITHUB_ACTIONS=true, or -Force is given.

.PARAMETER InstallerA  Installer of version A (the version under test).
.PARAMETER InstallerB  Optional installer of a higher version, used for the upgrade step.
.PARAMETER VersionA    Expected DisplayVersion after installing A.
.PARAMETER VersionB    Expected DisplayVersion after upgrading to B.
.PARAMETER LogDir      Where logs and summary.md are written.
#>
param(
  [Parameter(Mandatory = $true)][string]$InstallerA,
  [string]$InstallerB = '',
  [Parameter(Mandatory = $true)][string]$VersionA,
  [string]$VersionB = '',
  [string]$LogDir = (Join-Path $PSScriptRoot '..\installer-smoke-logs'),
  [int]$RunSeconds = 10,
  [switch]$Force
)

$ErrorActionPreference = 'Stop'

if (-not $Force -and $env:CI -ne 'true' -and $env:GITHUB_ACTIONS -ne 'true') {
  Write-Error 'Refusing to run: this installs a Windows service. Run it only on a CI runner (or pass -Force on a disposable VM).'
  exit 2
}
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  Write-Error 'Must run elevated (administrator).'
  exit 2
}

New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
$LogDir = (Resolve-Path $LogDir).Path
$InstallerA = (Resolve-Path $InstallerA).Path
if ($InstallerB) { $InstallerB = (Resolve-Path $InstallerB).Path }

$AppName = 'Ferrum'
$ServiceName = 'FerrumIndexer'
$InstallDir = Join-Path $env:ProgramFiles $AppName
$results = New-Object System.Collections.Generic.List[object]
$script:failed = $false

function Check([string]$Name, [bool]$Ok, [string]$Detail = '') {
  $status = if ($Ok) { 'PASS' } else { 'FAIL' }
  $line = "[$status] $Name"
  if ($Detail) { $line += " - $Detail" }
  Write-Host $line
  $results.Add([pscustomobject]@{ Status = $status; Name = $Name; Detail = $Detail })
  if (-not $Ok) { $script:failed = $true }
}

function Write-Summary {
  $md = New-Object System.Collections.Generic.List[string]
  $md.Add('## Installer smoke test')
  $md.Add('')
  $md.Add('| Result | Check | Detail |')
  $md.Add('|---|---|---|')
  foreach ($r in $results) { $md.Add("| $($r.Status) | $($r.Name) | $(($r.Detail -replace '\|','/')) |") }
  $text = $md -join "`n"
  Set-Content -Path (Join-Path $LogDir 'summary.md') -Value $text -Encoding utf8
  if ($env:GITHUB_STEP_SUMMARY) { Add-Content -Path $env:GITHUB_STEP_SUMMARY -Value $text -Encoding utf8 }
  Write-Host ''
  Write-Host '===== SUMMARY ====='
  foreach ($r in $results) { Write-Host ('{0,-5} {1}' -f $r.Status, $r.Name) }
  $pass = @($results | Where-Object Status -eq 'PASS').Count
  $fail = @($results | Where-Object Status -eq 'FAIL').Count
  Write-Host "PASS: $pass  FAIL: $fail"
}

function Fail-Now([string]$Message) {
  Check $Message $false
  Write-Summary
  exit 1
}

# Uninstall registry entries for the app (64-bit, 32-bit and per-user views).
function Get-ArpEntries {
  $roots = @(
    'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall',
    'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall',
    'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall'
  )
  foreach ($root in $roots) {
    if (-not (Test-Path $root)) { continue }
    foreach ($key in Get-ChildItem $root -ErrorAction SilentlyContinue) {
      $p = Get-ItemProperty $key.PSPath -ErrorAction SilentlyContinue
      if ($p -and $p.DisplayName -eq $AppName) {
        [pscustomobject]@{
          Key = $key.PSPath; DisplayVersion = $p.DisplayVersion
          UninstallString = $p.UninstallString; InstallLocation = $p.InstallLocation
        }
      }
    }
  }
}

function Get-StartMenuShortcuts {
  $dirs = @(
    (Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs'),
    (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs')
  )
  foreach ($d in $dirs) {
    if (Test-Path $d) {
      Get-ChildItem $d -Recurse -Filter '*.lnk' -ErrorAction SilentlyContinue |
        Where-Object { $_.BaseName -like "$AppName*" }
    }
  }
}

function Get-ServiceState {
  $out = & "$env:SystemRoot\System32\sc.exe" query $ServiceName 2>&1 | Out-String
  if ($LASTEXITCODE -ne 0) { return $null }
  if ($out -match 'STATE\s*:\s*\d+\s+(\w+)') { return $Matches[1] }
  return 'UNKNOWN'
}

function Run-Installer([string]$Exe, [string]$Label) {
  Write-Host ''
  Write-Host "--- $Label : $Exe /S ---"
  $p = Start-Process -FilePath $Exe -ArgumentList '/S' -PassThru -Wait
  return $p.ExitCode
}

function Wait-Until([scriptblock]$Cond, [int]$TimeoutSec = 120) {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
    if (& $Cond) { return $true }
    Start-Sleep -Seconds 2
  }
  return [bool](& $Cond)
}

function Verify-Install([string]$Version, [string]$Phase) {
  $exe = Join-Path $InstallDir "$AppName.exe"
  Check "$Phase`: Ferrum.exe present in Program Files" (Test-Path $exe) $exe
  Check "$Phase`: ferrum-indexer.exe present" (Test-Path (Join-Path $InstallDir 'ferrum-indexer.exe'))
  Check "$Phase`: uninstall.exe present" (Test-Path (Join-Path $InstallDir 'uninstall.exe'))
  $arp = @(Get-ArpEntries)
  Check "$Phase`: exactly one Add/Remove Programs entry" ($arp.Count -eq 1) "found $($arp.Count)"
  if ($arp.Count -ge 1) {
    Check "$Phase`: DisplayVersion = $Version" ($arp[0].DisplayVersion -eq $Version) "actual '$($arp[0].DisplayVersion)'"
    Check "$Phase`: UninstallString set" ([bool]$arp[0].UninstallString) $arp[0].UninstallString
  }
  $lnk = @(Get-StartMenuShortcuts)
  Check "$Phase`: Start Menu shortcut" ($lnk.Count -ge 1) (($lnk | ForEach-Object FullName) -join '; ')
  $state = Get-ServiceState
  Check "$Phase`: service $ServiceName installed" ($null -ne $state) "state=$state"
  Write-Host "[INFO] $Phase : service state = $state"
}

try {
  # Preconditions: clean machine.
  Check 'precondition: no previous Ferrum install' ((@(Get-ArpEntries).Count -eq 0) -and -not (Test-Path $InstallDir))
  Check 'precondition: service absent' ($null -eq (Get-ServiceState))
  if ($script:failed) { Write-Summary; exit 1 }

  # (a) install A
  $rc = Run-Installer $InstallerA "install A ($VersionA)"
  Check 'install A: installer exit code 0' ($rc -eq 0) "exit $rc"
  if ($rc -ne 0) { Fail-Now 'install A failed, aborting' }
  Verify-Install $VersionA 'install A'

  # (b) run the app
  $exe = Join-Path $InstallDir "$AppName.exe"
  $proc = Start-Process -FilePath $exe -PassThru
  Start-Sleep -Seconds $RunSeconds
  $alive = $false
  try { $proc.Refresh(); $alive = -not $proc.HasExited } catch { $alive = $false }
  $exitInfo = if ($alive) { 'running' } else { "exited with $($proc.ExitCode)" }
  Check "run: Ferrum.exe alive after ${RunSeconds}s" $alive $exitInfo
  Get-Process -Name $AppName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  Get-Process -Name 'msedgewebview2' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  Start-Sleep -Seconds 2
  Check 'run: process stopped' ($null -eq (Get-Process -Name $AppName -ErrorAction SilentlyContinue))

  # (c) upgrade A -> B
  if ($InstallerB) {
    if (-not $VersionB) { Fail-Now 'VersionB is required with InstallerB' }
    $stateBefore = Get-ServiceState
    $rc = Run-Installer $InstallerB "upgrade to B ($VersionB)"
    Check 'upgrade: installer exit code 0' ($rc -eq 0) "exit $rc"
    if ($rc -ne 0) { Fail-Now 'upgrade failed, aborting' }
    Verify-Install $VersionB 'upgrade'
    $stateAfter = Get-ServiceState
    Check 'upgrade: service state not worse than before' ($stateBefore -ne 'RUNNING' -or $stateAfter -eq 'RUNNING') "before=$stateBefore after=$stateAfter"
  } else {
    Write-Host '[INFO] upgrade step skipped (no InstallerB)'
  }

  # (d) uninstall via UninstallString
  $arp = @(Get-ArpEntries)
  if ($arp.Count -lt 1 -or -not $arp[0].UninstallString) { Fail-Now 'uninstall: no UninstallString to run' }
  $us = $arp[0].UninstallString.Trim()
  $uexe = if ($us.StartsWith('"')) { $us.Substring(1, $us.IndexOf('"', 1) - 1) } else { ($us -split ' /')[0] }
  Write-Host ''
  Write-Host "--- uninstall : $uexe /S ---"
  # The NSIS uninstaller may relaunch itself from %TEMP%, so the first process can exit early.
  $p = Start-Process -FilePath $uexe -ArgumentList '/S' -PassThru -Wait
  Check 'uninstall: uninstaller exit code 0' ($p.ExitCode -eq 0) "exit $($p.ExitCode)"
  $null = Wait-Until { (-not (Test-Path (Join-Path $InstallDir "$AppName.exe"))) -and (@(Get-ArpEntries).Count -eq 0) -and ($null -eq (Get-ServiceState)) } 120
  Start-Sleep -Seconds 3
  Check 'uninstall: Ferrum.exe removed' (-not (Test-Path (Join-Path $InstallDir "$AppName.exe")))
  Check 'uninstall: ferrum-indexer.exe removed' (-not (Test-Path (Join-Path $InstallDir 'ferrum-indexer.exe')))
  Check 'uninstall: Add/Remove Programs entry removed' (@(Get-ArpEntries).Count -eq 0)
  Check "uninstall: service $ServiceName removed" ($null -eq (Get-ServiceState))
  Check 'uninstall: Start Menu shortcut removed' (@(Get-StartMenuShortcuts).Count -eq 0)
  if (Test-Path $InstallDir) {
    $left = @(Get-ChildItem $InstallDir -Recurse -Force -ErrorAction SilentlyContinue | ForEach-Object FullName)
    Write-Host "[INFO] leftover in install dir: $($left -join '; ')"
  }
}
catch {
  Write-Host "Unhandled error: $_"
  Check "unhandled error: $_" $false
}
finally {
  Get-Process -Name $AppName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  try {
    $pd = Join-Path $env:ProgramData $AppName
    if (Test-Path $pd) {
      Get-ChildItem $pd -Recurse -File -ErrorAction SilentlyContinue | Select-Object FullName, Length |
        Out-File (Join-Path $LogDir 'programdata-listing.txt')
    }
  } catch {}
}

Write-Summary
if ($script:failed) { exit 1 }
exit 0
