#Requires -Version 5.1
# Install Kenji and ngrok as separate automatic Windows services.
# Run from an elevated PowerShell session. An existing ngrok configuration is
# required; this script deliberately never accepts or stores an auth token.
[CmdletBinding()]
param(
    [string]$NgrokConfig,
    [string]$KenjiExe,
    [string]$NgrokExe,
    [string]$InstallDir = (Join-Path $env:ProgramFiles 'Kenji'),
    [switch]$NoStart,
    [switch]$AllowLocalSystem
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw '管理者権限の PowerShell で実行してください。'
    }
}

function Resolve-ExistingFile([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label が見つかりません: $Path"
    }
    return (Resolve-Path -LiteralPath $Path).ProviderPath
}

function Resolve-Executable([string]$Explicit, [string]$CommandName, [string[]]$Candidates) {
    if (-not [string]::IsNullOrWhiteSpace($Explicit)) {
        return Resolve-ExistingFile $Explicit $CommandName
    }
    foreach ($candidate in $Candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return (Resolve-Path -LiteralPath $candidate).ProviderPath
        }
    }
    $command = Get-Command -Name $CommandName -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -ne $command) {
        return Resolve-ExistingFile $command.Source $CommandName
    }
    throw "$CommandName が見つかりません。実行ファイルの絶対パスを指定してください。"
}

function Assert-ExitCode([string]$Operation) {
    if ($LASTEXITCODE -ne 0) {
        throw "$Operation に失敗しました (exit code: $LASTEXITCODE)。"
    }
}

function Set-ServiceRecovery([string]$Name) {
    & "$env:SystemRoot\System32\sc.exe" config $Name 'start=' 'auto' | Out-Null
    Assert-ExitCode "$Name の自動起動設定"
    & "$env:SystemRoot\System32\sc.exe" failure $Name 'reset=' '86400' 'actions=' 'restart/60000/restart/60000/restart/60000' | Out-Null
    Assert-ExitCode "$Name の障害復旧設定"
}

function Start-IfStopped([string]$Name) {
    $service = Get-Service -Name $Name -ErrorAction Stop
    if ($service.Status -ne [System.ServiceProcess.ServiceControllerStatus]::Running) {
        Start-Service -Name $Name -ErrorAction Stop
        (Get-Service -Name $Name).WaitForStatus(
            [System.ServiceProcess.ServiceControllerStatus]::Running,
            [TimeSpan]::FromSeconds(30)
        )
    }
    Write-Host "$Name : Running"
}

Assert-Administrator
$kenjiService = Get-Service -Name 'kenji' -ErrorAction SilentlyContinue
$ngrokService = Get-Service -Name 'ngrok' -ErrorAction SilentlyContinue
$config = $null
if (-not [string]::IsNullOrWhiteSpace($NgrokConfig)) {
    $config = Resolve-ExistingFile $NgrokConfig 'ngrok 設定ファイル'
    if ([IO.Path]::GetExtension($config) -notin @('.yml', '.yaml')) {
        throw 'ngrok 設定ファイルには .yml または .yaml を指定してください。'
    }
} elseif ($null -eq $ngrokService) {
    throw 'ngrok サービスを初回登録するには -NgrokConfig が必要です。'
}
if ($null -eq $kenjiService -and -not $AllowLocalSystem) {
    throw 'Kenji の新規サービス登録は既定でLocalSystemになります。専用アカウントで先にサービスを登録するか、権限リスクを理解したうえで -AllowLocalSystem を付けてください。'
}

# Verify all external inputs before creating either service.
if ($null -eq $kenjiService) {
    $candidatePaths = @(
        (Join-Path $PSScriptRoot '..\target\release\kenji.exe'),
        (Join-Path $PSScriptRoot '..\server\target\release\kenji.exe'),
        (Join-Path $PSScriptRoot '..\target\debug\kenji.exe'),
        (Join-Path (Get-Location).Path 'kenji.exe')
    )
    $sourceExe = Resolve-Executable $KenjiExe 'kenji.exe' $candidatePaths
    $destinationExe = Join-Path $InstallDir 'kenji.exe'
}
if ($null -eq $ngrokService) {
    $ngrokBinary = Resolve-Executable $NgrokExe 'ngrok.exe' @()
}

if ($null -eq $kenjiService) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    if (-not [string]::Equals($sourceExe, $destinationExe, [StringComparison]::OrdinalIgnoreCase)) {
        Copy-Item -LiteralPath $sourceExe -Destination $destinationExe -Force
    }

    $authFile = Join-Path $env:ProgramData 'kenji\auth.json'
    if (-not (Test-Path -LiteralPath $authFile -PathType Leaf)) {
        Write-Host 'Kenji の初期管理者アカウントを作成します。'
        & $destinationExe setup
        Assert-ExitCode 'Kenji setup'
        if (-not (Test-Path -LiteralPath $authFile -PathType Leaf)) {
            throw 'Kenji の初期設定が完了していません。'
        }
    }

    & $destinationExe service install
    Assert-ExitCode 'Kenji のサービス登録'
} else {
    Write-Host 'Kenji サービスは登録済みです。既存の実行ファイルと設定を保持します。'
}

if ($null -eq $ngrokService) {
    # The official ngrok CLI installs its own native Windows service.
    # Reuse the existing config: never copy credentials into Kenji's config.json.
    & $ngrokBinary service install --config $config
    Assert-ExitCode 'ngrok のサービス登録'
} else {
    Write-Warning 'ngrok サービスは登録済みです。既存のサービス設定を変更しません。'
}

foreach ($name in @('kenji', 'ngrok')) {
    if ($null -eq (Get-Service -Name $name -ErrorAction SilentlyContinue)) {
        throw "$name のサービス登録を確認できません。"
    }
    Set-ServiceRecovery $name
}

if (-not $NoStart) {
    # ngrok may return a temporary upstream error before Kenji starts; start
    # the application first so a working upstream exists before the tunnel.
    Start-IfStopped 'kenji'
    Start-IfStopped 'ngrok'
}

Write-Host 'Kenji と ngrok の自動起動・障害復旧設定が完了しました。'
Write-Host '状態確認: Get-Service kenji,ngrok'
Write-Warning 'Kenji を LocalSystem で登録した場合、遠隔コマンドも SYSTEM 権限で実行されます。専用の低権限サービスアカウントへの変更を推奨します。'
Write-Warning 'ngrok の公開先にはアクセス制限を設定してください。'
