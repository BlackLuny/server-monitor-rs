#Requires -Version 5.1
#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Installer for the server-monitor-rs agent on Windows.

.DESCRIPTION
    Lays the agent out at C:\ProgramData\monitor-agent (overrideable),
    writes agent.yaml via `monitor-agent.exe configure`, and registers a
    Windows Service so the agent starts at boot and restarts on failure.

    Source modes (pick one):
      -LocalBinary <path>   Copy a pre-built monitor-agent.exe.
      -ReleaseUrl <base>    Download <base>/<version>/monitor-agent-x86_64-pc-windows-msvc.zip

    Falls back to a Scheduled Job if `sc.exe create` is unavailable for any
    reason (rare — only used when running on a stripped-down host).

.EXAMPLE
    PS> Set-ExecutionPolicy -Scope Process Bypass -Force
    PS> .\install-agent.ps1 -Endpoint https://panel.example.com/grpc `
                            -Token <join-token> `
                            -LocalBinary .\monitor-agent.exe

.EXAMPLE
    PS> iwr -useb https://example/install-agent.ps1 | iex; `
        Install-MonitorAgent -Endpoint https://panel.example.com/grpc -Token xxxx
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $false)] [string] $Endpoint = "",
    [Parameter(Mandatory = $false)] [string] $Token = "",
    [Parameter(Mandatory = $false)] [string] $LocalBinary = "",
    [Parameter(Mandatory = $false)] [string] $ReleaseUrl = "",
    [Parameter(Mandatory = $false)] [string] $Version = "latest",
    [Parameter(Mandatory = $false)] [int]    $Heartbeat = 10,
    [Parameter(Mandatory = $false)] [string] $InstallRoot = "$env:ProgramData\monitor-agent",
    [Parameter(Mandatory = $false)] [string] $ServiceName = "monitor-agent",
    [Parameter(Mandatory = $false)] [switch] $SkipService,
    [Parameter(Mandatory = $false)] [switch] $UseScheduledTask,
    [Parameter(Mandatory = $false)] [switch] $DryRun
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Write-Info($msg)  { Write-Host "==> $msg"  -ForegroundColor Green }
function Write-Warn($msg)  { Write-Host "!!! $msg"  -ForegroundColor Yellow }
function Write-Fatal($msg) { Write-Host "xxx $msg" -ForegroundColor Red; exit 1 }

function Run([string]$cmd, [string[]]$args) {
    if ($DryRun) {
        Write-Host "+ $cmd $($args -join ' ')"
        return
    }
    & $cmd @args
    if ($LASTEXITCODE -ne 0) { Write-Fatal "command failed: $cmd $($args -join ' ')" }
}

# -----------------------------------------------------------------------------
# Resolve required values (PROMPT only when running interactively).
# -----------------------------------------------------------------------------
if (-not $Endpoint) {
    if ([Environment]::UserInteractive -and -not [Console]::IsInputRedirected) {
        $Endpoint = Read-Host "Panel endpoint (e.g. https://panel.example.com/grpc)"
    } else {
        Write-Fatal "missing -Endpoint (running non-interactively)"
    }
}
if (-not $Token) {
    if ([Environment]::UserInteractive -and -not [Console]::IsInputRedirected) {
        $Token = Read-Host "Join token"
    } else {
        Write-Fatal "missing -Token (running non-interactively)"
    }
}

# -----------------------------------------------------------------------------
# Directory layout
# -----------------------------------------------------------------------------
$BinDir       = Join-Path $InstallRoot "bin"
$ConfigDir    = Join-Path $InstallRoot "config"
$DataDir      = Join-Path $InstallRoot "data"
$LogDir       = Join-Path $DataDir     "logs"
$RecordingDir = Join-Path $DataDir     "recordings"

foreach ($d in @($BinDir, $ConfigDir, $DataDir, $LogDir, $RecordingDir)) {
    if (-not (Test-Path $d)) {
        if ($DryRun) { Write-Host "+ mkdir $d" }
        else { New-Item -ItemType Directory -Path $d -Force | Out-Null }
    }
}

$BinPath    = Join-Path $BinDir    "monitor-agent.exe"
$ConfigPath = Join-Path $ConfigDir "agent.yaml"

# -----------------------------------------------------------------------------
# Acquire binary
# -----------------------------------------------------------------------------
$Tmp = Join-Path $env:TEMP "monitor-agent-install-$([Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $Tmp -Force | Out-Null
try {
    if ($LocalBinary) {
        if (-not (Test-Path $LocalBinary)) { Write-Fatal "-LocalBinary $LocalBinary not found" }
        Write-Info "using local binary $LocalBinary"
        Copy-Item -Force $LocalBinary $BinPath
    } else {
        if (-not $ReleaseUrl) {
            Write-Fatal "no -ReleaseUrl configured yet — pass -LocalBinary <path> or -ReleaseUrl <base> (M7 will set a default)"
        }
        $arch = if ([Environment]::Is64BitOperatingSystem) { "x86_64" } else { "i686" }
        $triple = "${arch}-pc-windows-msvc"
        $url = "$($ReleaseUrl.TrimEnd('/'))/$Version/monitor-agent-$triple.zip"
        $zip = Join-Path $Tmp "agent.zip"
        Write-Info "downloading $url"
        if (-not $DryRun) {
            Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
            Expand-Archive -Force -Path $zip -DestinationPath $Tmp
            $exe = Get-ChildItem -Path $Tmp -Recurse -Filter "monitor-agent.exe" | Select-Object -First 1
            if (-not $exe) { Write-Fatal "monitor-agent.exe not found in archive" }
            Copy-Item -Force $exe.FullName $BinPath
        }
    }

    # -------------------------------------------------------------------------
    # agent.yaml — written by the binary's own `configure` so the format
    # stays canonical even when fields evolve.
    # -------------------------------------------------------------------------
    Write-Info "writing $ConfigPath"
    $env:MONITOR_AGENT_CONFIG = $ConfigPath
    Run $BinPath @("configure", "--endpoint", $Endpoint, "--token", $Token, "--heartbeat", "$Heartbeat")

    if (-not $DryRun -and (Test-Path $ConfigPath)) {
        # Lock the config file down to admins + SYSTEM. Removes inherited ACL
        # so non-admin users on the box can't read the join token.
        $acl = Get-Acl $ConfigPath
        $acl.SetAccessRuleProtection($true, $false)
        $admins  = New-Object System.Security.Principal.NTAccount("BUILTIN\Administrators")
        $system  = New-Object System.Security.Principal.NTAccount("NT AUTHORITY\SYSTEM")
        foreach ($p in @($admins, $system)) {
            $rule = New-Object System.Security.AccessControl.FileSystemAccessRule(
                $p, "FullControl", "Allow")
            $acl.AddAccessRule($rule)
        }
        Set-Acl -Path $ConfigPath -AclObject $acl
    }

    # -------------------------------------------------------------------------
    # Service registration
    # -------------------------------------------------------------------------
    if ($SkipService) {
        Write-Info "skipping service setup (-SkipService)"
    } elseif ($UseScheduledTask) {
        Write-Info "registering scheduled task $ServiceName"
        $action  = New-ScheduledTaskAction -Execute $BinPath -Argument "run" `
                       -WorkingDirectory $BinDir
        $trigger = New-ScheduledTaskTrigger -AtStartup
        $princ   = New-ScheduledTaskPrincipal -UserId "SYSTEM" -RunLevel Highest
        $set     = New-ScheduledTaskSettingsSet -RestartCount 5 -RestartInterval (New-TimeSpan -Seconds 30)
        if (-not $DryRun) {
            Register-ScheduledTask -TaskName $ServiceName -Action $action -Trigger $trigger `
                                   -Principal $princ -Settings $set -Force | Out-Null
            Start-ScheduledTask -TaskName $ServiceName
        }
    } else {
        Write-Info "registering service $ServiceName"
        # `sc.exe create` accepts the binary path with arguments via `binPath=`.
        # Quote whole binPath value because Service Control parses on spaces.
        $binPathArg = "`"$BinPath`" run"
        # Ensure the service env var is set before the service starts. Use
        # SetX so the variable persists across boots for the SYSTEM scope.
        Run "setx" @("/M", "MONITOR_AGENT_CONFIG", $ConfigPath)
        # Stop+delete an existing service so re-runs are idempotent.
        $existing = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
        if ($existing) {
            if ($existing.Status -eq "Running") { Run "sc.exe" @("stop", $ServiceName) }
            Run "sc.exe" @("delete", $ServiceName)
            Start-Sleep -Seconds 1
        }
        Run "sc.exe" @("create", $ServiceName, "binPath=", $binPathArg, "start=", "auto",
                       "DisplayName=", "server-monitor-rs agent")
        Run "sc.exe" @("description", $ServiceName,
                       "Forwards system metrics + probe results + Web SSH to the panel.")
        Run "sc.exe" @("failure", $ServiceName, "reset=", "60", "actions=", "restart/5000/restart/5000/restart/5000")
        Run "sc.exe" @("start", $ServiceName)
    }

    Write-Host ""
    Write-Host "✅ monitor-agent installed." -ForegroundColor Green
    Write-Host "Binary:   $BinPath"
    Write-Host "Config:   $ConfigPath"
    Write-Host "Records:  $RecordingDir"
    if (-not $SkipService) {
        if ($UseScheduledTask) {
            Write-Host "Manage:   Get-ScheduledTask -TaskName $ServiceName | Start-/Stop-ScheduledTask"
        } else {
            Write-Host "Manage:   Get-Service $ServiceName | Start-/Stop-/Restart-Service"
            Write-Host "Logs:     Get-EventLog -LogName Application -Source $ServiceName"
        }
    }
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
