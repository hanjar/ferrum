<#
.SYNOPSIS
  FER-474 (+ FER-476, FER-480): read-only checks of an installed FerrumIndexer service (manual verification).

.DESCRIPTION
  Changes nothing. Run in an ELEVATED PowerShell after installing Ferrum with the MSI or NSIS
  installer (some queries, e.g. the ProgramData ACL, need admin). Prints each check with PASS/FAIL
  and the raw output so it can be pasted into the issue tracker.

  See docs/indexer-service.md section 6 for the full manual procedure.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\indexer-verify.ps1
#>
param(
  [string]$InstallDir = "$env:ProgramFiles\Ferrum"
)

$ErrorActionPreference = 'Continue'
$fail = 0
function Check([string]$name, [bool]$ok, [string]$detail) {
  $tag = if ($ok) { 'PASS' } else { 'FAIL'; $script:fail++ }
  Write-Host ("[{0}] {1}" -f $tag, $name)
  if ($detail) { Write-Host ($detail.Trim() -replace '(?m)^', '       ') }
}

$exe = Join-Path $InstallDir 'ferrum-indexer.exe'
Check 'binary installed next to ferrum.exe' (Test-Path $exe) $exe

$qc = (sc.exe qc FerrumIndexer 2>&1 | Out-String)
Check 'service registered' ($qc -match 'SERVICE_NAME: FerrumIndexer') $qc
Check 'runs as LocalSystem' ($qc -match 'SERVICE_START_NAME\s*:\s*LocalSystem') ''
Check 'auto start (delayed)' ($qc -match 'AUTO_START\s+\(DELAYED\)') ''
Check 'image path quoted, under Program Files' ($qc -match 'BINARY_PATH_NAME\s*:\s*"[^"]*\\Program Files\\Ferrum\\ferrum-indexer\.exe"') ''

$sid = (sc.exe qsidtype FerrumIndexer 2>&1 | Out-String)
# FER-476: RESTRICTED is the default; UNRESTRICTED only when install's probe failed (see indexer.log).
Check 'service SID type RESTRICTED' ($sid -match 'SERVICE_SID_TYPE:\s*RESTRICTED') $sid

$privs = (sc.exe qprivs FerrumIndexer 2>&1 | Out-String)
Check 'RequiredPrivileges = SeChangeNotifyPrivilege only' (($privs -match 'SeChangeNotifyPrivilege') -and -not ($privs -match 'SeDebug|SeTcb|SeBackup|SeRestore|SeTakeOwnership|SeImpersonate|SeLoadDriver')) $privs

$fa = (sc.exe qfailure FerrumIndexer 2>&1 | Out-String)
Check 'recovery: restart on failure' ($fa -match 'RESTART') $fa

$q = (sc.exe query FerrumIndexer 2>&1 | Out-String)
Check 'service RUNNING' ($q -match 'STATE\s*:\s*4\s+RUNNING') $q

# Effective privileges of the running process (needs admin; best effort).
$svc = Get-CimInstance Win32_Service -Filter "Name='FerrumIndexer'" -ErrorAction SilentlyContinue
if ($svc -and $svc.ProcessId) {
  Check 'service process id' $true ("PID {0}" -f $svc.ProcessId)
}

$dir = Join-Path $env:ProgramData 'Ferrum'
$acl = (icacls.exe $dir 2>&1 | Out-String)
$aclOk = ($acl -match 'NT AUTHORITY\\SYSTEM:\(OI\)\(CI\)\(F\)') -and ($acl -match 'BUILTIN\\Administrators:\(OI\)\(CI\)\(F\)') -and -not ($acl -match 'BUILTIN\\Users|Everyone|Authenticated Users|CREATOR OWNER')
Check 'ProgramData\Ferrum ACL = SYSTEM + Administrators (+ NT SERVICE\FerrumIndexer) only' $aclOk $acl
$owner = (Get-Acl $dir -ErrorAction SilentlyContinue).Owner
Check 'ProgramData\Ferrum owner = Administrators' ($owner -eq 'BUILTIN\Administrators') $owner
$conf = Join-Path $dir 'indexer.conf'
Check 'indexer.conf present' (Test-Path $conf) ((Get-Content $conf -ErrorAction SilentlyContinue) -join "`n")

# FER-480: the pipe is \\.\pipe\FerrumIndexer (NPFS root; a Restricted service cannot create pipes
# under ProtectedPrefix\Administrators). Owner SYSTEM (explicit O:SY, FER-480 review F2); DACL has
# no Everyone/Users ACE; and the process serving it is the service (the check every client makes
# before sending anything).
$pipe = '\\.\pipe\FerrumIndexer'
$pipeAcl = $null
try { $pipeAcl = [System.IO.Directory]::GetAccessControl($pipe) } catch { }
if ($pipeAcl) {
  $sddl = $pipeAcl.GetSecurityDescriptorSddlForm('All')
  Check 'pipe owner SYSTEM (explicit O:SY, FER-480 review F2)' ($sddl -match '^O:SY') $sddl
  Check 'pipe DACL: SY + service SID full, IU data-only (0x120183), NU/AN denied' (($sddl -match '\(A;;0x120183;;;IU\)') -and ($sddl -match '\(D;;FA;;;NU\)|\(D;;GA;;;NU\)') -and ($sddl -match 'S-1-5-80-401986193-3732369529-2668797022-3469762217-2719989880') -and -not ($sddl -match ';;;(WD|BU|AU)\)')) ''
} else {
  Write-Host '[INFO] pipe ACL not readable from PowerShell; use the status command below instead.'
}
Add-Type -Namespace FerVerify -Name Pipe -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
public static extern IntPtr CreateFileW(string n, uint a, uint s, IntPtr sa, uint c, uint f, IntPtr t);
[DllImport("kernel32.dll", SetLastError=true)]
public static extern bool GetNamedPipeServerProcessId(IntPtr h, out uint pid);
[DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
'@
# Client rights only (0x120183), identification-level SQOS; sends nothing.
$h = [FerVerify.Pipe]::CreateFileW($pipe, 0x00120183, 0, [IntPtr]::Zero, 3, 0x00110000, [IntPtr]::Zero)
if ($h -ne [IntPtr]-1) {
  $spid = [uint32]0
  $okPid = [FerVerify.Pipe]::GetNamedPipeServerProcessId($h, [ref]$spid)
  [void][FerVerify.Pipe]::CloseHandle($h)
  $scmPid = if ($svc) { $svc.ProcessId } else { 0 }
  Check 'pipe served by the service process (server PID = SCM PID)' ($okPid -and $spid -eq $scmPid -and $spid -ne 0) ("pipe server PID {0}, SCM PID {1}" -f $spid, $scmPid)
} else {
  Check 'pipe \\.\pipe\FerrumIndexer exists' $false ("open failed: Win32 error {0}" -f [Runtime.InteropServices.Marshal]::GetLastWin32Error())
}

# FER-480: the install probe must pass, and the service must listen on the new pipe.
$log = Join-Path $dir 'indexer.log'
$tail = (Get-Content $log -Tail 200 -ErrorAction SilentlyContinue)
$lastProbe = ($tail | Select-String 'restricted service SID probe' | Select-Object -Last 1)
Check 'last install probe passed (no "probe failed")' ($lastProbe -and -not ($lastProbe.Line -match 'probe failed')) ($lastProbe | Out-String)
$lastListen = ($tail | Select-String 'listening on' | Select-Object -Last 1)
# FER-487: the fixed name, or the announced fallback name when the fixed one was taken.
Check 'service listens on the fixed or the announced pipe' ($lastListen -and ($lastListen.Line -match 'listening on \\\\\.\\pipe\\FerrumIndexer(-[0-9a-f]{32})? \(owner')) ($lastListen | Out-String)

$st = (& $exe status 2>&1 | Out-String)
Check 'status over the pipe' ($LASTEXITCODE -eq 0 -and $st -match 'state=running') $st
# FER-476 review H1: queries are only served with a restricted service token.
Check 'queries enabled (not degraded)' ($st -match 'degraded=false') $st

# FER-475 (S2): the index.
$ixDir = Join-Path $dir 'index'
if (Test-Path $ixDir) {
  $ixAcl = (icacls.exe $ixDir 2>&1 | Out-String)
  $ixOk = ($ixAcl -match 'NT AUTHORITY\\SYSTEM:\(OI\)\(CI\)\(F\)') -and -not ($ixAcl -match 'BUILTIN\\Users|Everyone|Authenticated Users|CREATOR OWNER')
  Check 'ProgramData\Ferrum\index ACL = SYSTEM + Administrators (+ service SID) only' $ixOk $ixAcl
  $bad = Get-ChildItem $ixDir -Force -ErrorAction SilentlyContinue | Where-Object { $_.Name -notmatch '^vol-[0-9A-F]{16}\.idx$' }
  Check 'index folder holds only vol-<serial>.idx files' (-not $bad) (($bad | ForEach-Object Name) -join ', ')
} else {
  Write-Host '[INFO] no index folder yet (first build still running?) - rerun in a minute.'
}
$ix = (& $exe index-status 2>&1 | Out-String)
Check 'index-status over the pipe' ($LASTEXITCODE -eq 0 -and $ix -match 'volumes=') $ix
Check 'no volume in error' (-not ($ix -match 'state=error')) ''

# FER-487: the same trust rule as announce::key_is_trusted, for a key's SDDL.
# Owner SYSTEM or Administrators; a DACL must exist; no ALLOW ACE that applies to
# the key itself (INHERIT_ONLY 'IO' ACEs are skipped) grants a write right to a SID
# other than SYSTEM, Administrators or the service SID.
function Test-KeyTrusted([string]$sddl) {
  if ($sddl -notmatch '^O:(BA|SY)') { return $false }
  if ($sddl -notmatch 'D:') { return $false }
  $dacl = $sddl.Substring($sddl.IndexOf('D:'))
  $svcSid = 'S-1-5-80-401986193-3732369529-2668797022-3469762217-2719989880'
  $writeRights = @('KA', 'KW', 'GA', 'GW', 'WD', 'WO', 'SD', 'DE')
  foreach ($m in [regex]::Matches($dacl, '\(A;([A-Z]*);([A-Z]+);;;([A-Z0-9\-]+)\)')) {
    if ($m.Groups[1].Value -match 'IO') { continue }
    $sid = $m.Groups[3].Value
    if (@('SY', 'BA', $svcSid) -contains $sid) { continue }
    if ($writeRights -contains $m.Groups[2].Value) { return $false }
  }
  return $true
}
$svcKey = 'HKLM:\SYSTEM\CurrentControlSet\Services\FerrumIndexer'
$svcSddl = try { (Get-Acl -Path $svcKey -ErrorAction Stop).GetSecurityDescriptorSddlForm('All') } catch { '' }
Check 'service key (parent of Parameters): owner SYSTEM/Administrators, no write for other SIDs' (Test-KeyTrusted $svcSddl) $svcSddl

# FER-487: where the service announces its fallback pipe name. The key must be
# writable by SYSTEM, Administrators and the service SID only; PipeName is either
# absent (the fixed name is in use) or a valid fallback name.
$paramKey = 'HKLM:\SYSTEM\CurrentControlSet\Services\FerrumIndexer\Parameters'
$pKeySddl = try { (Get-Acl -Path $paramKey -ErrorAction Stop).GetSecurityDescriptorSddlForm('All') } catch { '' }
Check 'PipeName key: owner Administrators, Users read only, no write for AU/WD/IU' (($pKeySddl -match '^O:BA') -and ($pKeySddl -match '\(A;[A-Z]*CI[A-Z]*;KR;;;BU\)') -and -not ($pKeySddl -match ';;;(AU|WD|IU)\)')) $pKeySddl
$pn = (Get-ItemProperty -Path $paramKey -Name PipeName -ErrorAction SilentlyContinue).PipeName
$pnOk = (-not $pn) -or ($pn -match '^FerrumIndexer-[0-9a-f]{32}$')
Check 'PipeName is absent or a valid fallback name' $pnOk ("PipeName = {0}" -f $(if ($pn) { $pn } else { '(absent: fixed name in use)' }))
Write-Host ''
if ($fail -eq 0) { Write-Host 'ALL CHECKS PASSED' } else { Write-Host "$fail CHECK(S) FAILED" }
exit $fail
