<#
.SYNOPSIS
  FER-480 diagnostic: can a write-restricted token (shaped like a
  SERVICE_SID_TYPE_RESTRICTED service token) create named pipes, under
  \\.\pipe\ProtectedPrefix\Administrators\ and at the NPFS root?

.DESCRIPTION
  Read-only for the system: it creates throw-away pipes in this process,
  closes them, and changes nothing. No service, registry or file changes.

  For each token (this process's own token, and a write-restricted copy with
  restricting SIDs NT SERVICE\FerrumIndexer, Everyone, WRITE RESTRICTED, i.e.
  what the SCM gives a Restricted service) it tries to create the FIRST and a
  SECOND instance of:
    - \\.\pipe\ProtectedPrefix\Administrators\FerrumProbe-<pid>
    - \\.\pipe\FerrumProbe-<pid>
  with a DACL that grants the current user and the service SID full access.

  Run it twice: once normally, once from an elevated PowerShell. Expected
  (FER-480 hypothesis):
    elevated   own token        ProtectedPrefix first OK   root first OK
    elevated   write-restricted ProtectedPrefix first DENIED (5)  root OK
    standard   own token        ProtectedPrefix first DENIED (5)  root OK
    standard   write-restricted ProtectedPrefix first DENIED (5)  root OK
  i.e. the ProtectedPrefix check is a group-membership check that a
  write-restricted token fails (Administrators is not one of its restricting
  SIDs), independent of the pipe's own DACL.

  -Client additionally opens the installed service's pipe(s) as a client
  (sends nothing) and shows what a non-admin client can verify about the
  server process (PID from the pipe vs. PID from the SCM, image path, token).
#>
param([switch]$Client)

$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class FerProbe {
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr p, uint a, out IntPtr t);
  [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
  [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool ConvertStringSidToSidW(string s, out IntPtr sid);
  [StructLayout(LayoutKind.Sequential)] struct SA { public IntPtr Sid; public uint Attr; }
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool CreateRestrictedToken(IntPtr t, uint f, uint nd, IntPtr d, uint np, IntPtr p, uint nr, SA[] r, out IntPtr nt);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool DuplicateTokenEx(IntPtr t, uint a, IntPtr sa, int il, int tt, out IntPtr nt);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool SetThreadToken(IntPtr th, IntPtr t);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool RevertToSelf();
  [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool ConvertStringSecurityDescriptorToSecurityDescriptorW(string s, uint r, out IntPtr sd, IntPtr n);
  [StructLayout(LayoutKind.Sequential)] struct SECATTR { public int n; public IntPtr sd; public int inh; }
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern IntPtr CreateNamedPipeW(string n, uint om, uint pm, uint mi, uint ob, uint ib, uint to, ref SECATTR sa);
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern IntPtr CreateFileW(string n, uint a, uint s, IntPtr sa, uint c, uint f, IntPtr t);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetNamedPipeServerProcessId(IntPtr h, out uint pid);
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint a, bool i, uint pid);
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool QueryFullProcessImageNameW(IntPtr p, uint f, StringBuilder b, ref uint n);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool GetTokenInformation(IntPtr t, int c, IntPtr b, uint n, out uint r);
  [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool ConvertSidToStringSidW(IntPtr sid, out IntPtr s);
  [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);
  static readonly IntPtr INVALID = new IntPtr(-1);

  public static IntPtr RestrictedToken(string[] sids) {
    IntPtr baseTok; if (!OpenProcessToken(GetCurrentProcess(), 0x0002|0x0008|0x0001, out baseTok)) throw new Exception("OpenProcessToken " + Marshal.GetLastWin32Error());
    var r = new SA[sids.Length];
    for (int i = 0; i < sids.Length; i++) { IntPtr p; if (!ConvertStringSidToSidW(sids[i], out p)) throw new Exception("sid " + sids[i]); r[i].Sid = p; }
    IntPtr rt; if (!CreateRestrictedToken(baseTok, 0x8 /*WRITE_RESTRICTED*/, 0, IntPtr.Zero, 0, IntPtr.Zero, (uint)r.Length, r, out rt)) throw new Exception("CreateRestrictedToken " + Marshal.GetLastWin32Error());
    IntPtr imp; if (!DuplicateTokenEx(rt, 0x0004|0x0008, IntPtr.Zero, 2, 2, out imp)) throw new Exception("DuplicateTokenEx " + Marshal.GetLastWin32Error());
    CloseHandle(baseTok); CloseHandle(rt); return imp;
  }

  // Returns "first=OK|ERR n, second=OK|ERR n" for `name` created under `token` (IntPtr.Zero = own token).
  public static string TryPipes(string name, string sddl, IntPtr token) {
    IntPtr sd; if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, 1, out sd, IntPtr.Zero)) throw new Exception("sddl " + Marshal.GetLastWin32Error());
    var sa = new SECATTR { n = Marshal.SizeOf(typeof(SECATTR)), sd = sd, inh = 0 };
    if (token != IntPtr.Zero && !SetThreadToken(IntPtr.Zero, token)) throw new Exception("SetThreadToken " + Marshal.GetLastWin32Error());
    IntPtr a = INVALID, b = INVALID; int ea = 0, eb = 0;
    try {
      // PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE; byte mode | PIPE_REJECT_REMOTE_CLIENTS
      a = CreateNamedPipeW(name, 0x3 | 0x00080000, 0x8, 10, 4096, 4096, 0, ref sa); if (a == INVALID) ea = Marshal.GetLastWin32Error();
      if (a != INVALID) { b = CreateNamedPipeW(name, 0x3, 0x8, 10, 4096, 4096, 0, ref sa); if (b == INVALID) eb = Marshal.GetLastWin32Error(); }
    } finally { if (token != IntPtr.Zero) RevertToSelf(); }
    string res = "first=" + (a != INVALID ? "OK" : "ERR " + ea) + ", second=" + (a == INVALID ? "-" : (b != INVALID ? "OK" : "ERR " + eb));
    if (a != INVALID) CloseHandle(a); if (b != INVALID) CloseHandle(b); LocalFree(sd);
    return res;
  }

  // Client view of a running server pipe; sends nothing.
  public static string ClientView(string name) {
    // FILE_READ_DATA|FILE_WRITE_DATA|FILE_READ_ATTRIBUTES|FILE_WRITE_ATTRIBUTES|READ_CONTROL|SYNCHRONIZE, OPEN_EXISTING, SQOS identification
    IntPtr h = CreateFileW(name, 0x00120183, 0, IntPtr.Zero, 3, 0x00100000 | 0x00010000, IntPtr.Zero);
    if (h == INVALID) return "open: ERR " + Marshal.GetLastWin32Error();
    var sb = new StringBuilder();
    uint pid; if (GetNamedPipeServerProcessId(h, out pid)) sb.Append("serverPid=" + pid); else sb.Append("serverPid ERR " + Marshal.GetLastWin32Error());
    CloseHandle(h);
    IntPtr p = OpenProcess(0x1000 /*PROCESS_QUERY_LIMITED_INFORMATION*/, false, pid);
    if (p == IntPtr.Zero) { sb.Append("; OpenProcess ERR " + Marshal.GetLastWin32Error()); return sb.ToString(); }
    var img = new StringBuilder(1024); uint n = 1024;
    if (QueryFullProcessImageNameW(p, 0, img, ref n)) sb.Append("; image=" + img); else sb.Append("; image ERR " + Marshal.GetLastWin32Error());
    IntPtr t; if (!OpenProcessToken(p, 0x0008, out t)) { sb.Append("; OpenProcessToken ERR " + Marshal.GetLastWin32Error()); CloseHandle(p); return sb.ToString(); }
    uint need; GetTokenInformation(t, 2 /*TokenGroups*/, IntPtr.Zero, 0, out need);
    IntPtr buf = Marshal.AllocHGlobal((int)need);
    if (GetTokenInformation(t, 2, buf, need, out need)) {
      int count = Marshal.ReadInt32(buf); int ptrSize = IntPtr.Size; int entry = ptrSize * 2;
      for (int i = 0; i < count; i++) {
        IntPtr sid = Marshal.ReadIntPtr(buf, ptrSize + i * entry);
        IntPtr s; if (ConvertSidToStringSidW(sid, out s)) { string ss = Marshal.PtrToStringUni(s); LocalFree(s); if (ss.StartsWith("S-1-5-80-")) sb.Append("; serviceSid=" + ss); }
      }
    } else sb.Append("; TokenGroups ERR " + Marshal.GetLastWin32Error());
    Marshal.FreeHGlobal(buf); CloseHandle(t); CloseHandle(p);
    return sb.ToString();
  }
}
'@

$svcSid = 'S-1-5-80-401986193-3732369529-2668797022-3469762217-2719989880' # NT SERVICE\FerrumIndexer
$me = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$elevated = (New-Object System.Security.Principal.WindowsPrincipal($me)).IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)
$userSid = $me.User.Value
$sddl = "D:P(D;;GA;;;NU)(D;;GA;;;AN)(A;;GA;;;$userSid)(A;;GA;;;$svcSid)S:(ML;;NWNR;;;ME)"
"elevated: $elevated   user: $userSid"

$restricted = [FerProbe]::RestrictedToken(@($svcSid, 'S-1-1-0', 'S-1-5-33'))
$tag = $PID
foreach ($tok in @(@{n = 'own token'; h = [IntPtr]::Zero }, @{n = 'write-restricted'; h = $restricted })) {
  foreach ($p in @("\\.\pipe\ProtectedPrefix\Administrators\FerrumProbe-$tag", "\\.\pipe\FerrumProbe-$tag")) {
    "{0,-17} {1,-58} {2}" -f $tok.n, $p, [FerProbe]::TryPipes($p, $sddl, $tok.h)
  }
}

if ($Client) {
  $scm = (sc.exe queryex FerrumIndexer | Select-String 'PID\s*:\s*(\d+)').Matches
  "SCM FerrumIndexer PID: " + ($(if ($scm) { $scm[0].Groups[1].Value } else { '<not running>' }))
  foreach ($p in @('\\.\pipe\ProtectedPrefix\Administrators\FerrumIndexer', '\\.\pipe\FerrumIndexer')) {
    "client {0,-55} {1}" -f $p, [FerProbe]::ClientView($p)
  }
}
