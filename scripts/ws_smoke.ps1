# Kenji WebSocket terminal smoke test (PowerShell, no external deps)
# Usage: powershell -File scripts/ws_smoke.ps1 [token]
# Starts nothing; expects server running at 127.0.0.1:8787.

param([string]$Token = "", [string]$DataDir = "")

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$bin = Join-Path $root "target\debug\kenji.exe"
if (-not (Test-Path $bin)) { $bin = Join-Path $root "target\release\kenji.exe" }
if ($DataDir -eq "") { $DataDir = Join-Path $root ".data-smoke" }

if ($Token -eq "") {
  $login = Invoke-RestMethod -Method Post "http://127.0.0.1:8787/api/auth/login" -Body (@{username="admin";password="test123456"} | ConvertTo-Json) -ContentType "application/json"
  $Token = $login.token
}

Write-Host "Connecting to terminal WS..."
$ws = [System.Net.WebSockets.ClientWebSocket]::new()
$uri = [Uri]("ws://127.0.0.1:8787/api/terminal/ws?token=$Token")
$ws.ConnectAsync($uri, [Threading.CancellationToken]::None).Wait()
Write-Host "Connected. Sending resize + commands..."

function Send-Text([string]$s) {
  $bytes = [Text.Encoding]::UTF8.GetBytes($s)
  $seg = [ArraySegment[byte]]::new($bytes)
  $ws.SendAsync($seg, [System.Net.WebSockets.WebSocketMessageType]::Text, $true, [Threading.CancellationToken]::None).Wait()
}
function Send-Bin([string]$s) {
  $bytes = [Text.Encoding]::UTF8.GetBytes($s)
  $seg = [ArraySegment[byte]]::new($bytes)
  $ws.SendAsync($seg, [System.Net.WebSockets.WebSocketMessageType]::Binary, $true, [Threading.CancellationToken]::None).Wait()
}
function Read-Out([int]$ms) {
  $buf = New-Object byte[] 65536
  $seg = [ArraySegment[byte]]::new($buf)
  $task = $ws.ReceiveAsync($seg, [Threading.CancellationToken]::None)
  if (-not $task.Wait($ms)) { return $null }
  $r = $task.Result
  if ($r.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) { return "__CLOSE__" }
  return [Text.Encoding]::UTF8.GetString($buf, 0, $r.Count)
}

# initial banner
$null = Read-Out 1500
Send-Text '{"type":"resize","cols":100,"rows":30}'
Start-Sleep -Milliseconds 300
Send-Bin "echo KENJI_WS_OK`r`n"
$found = $false
$deadline = (Get-Date).AddSeconds(15)
while ((Get-Date) -lt $deadline) {
  $out = Read-Out 2000
  if ($null -eq $out) { continue }
  if ($out -eq "__CLOSE__") { break }
  if ($out -match "KENJI_WS_OK") { $found = $true; break }
}
if ($found) {
  Write-Host "PASS terminal echo via ConPTY verified" -ForegroundColor Green
  Send-Bin "exit`r`n"
  Start-Sleep -Milliseconds 500
  try { $ws.CloseAsync([System.Net.WebSockets.WebSocketCloseStatus]::NormalClosure, "bye", [Threading.CancellationToken]::None).Wait() } catch {}
  exit 0
} else {
  Write-Host "FAIL no echo through terminal" -ForegroundColor Red
  exit 1
}
