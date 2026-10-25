# Kenji API smoke test (PowerShell)
# Usage: powershell -File scripts/smoke.ps1
# Assumes: server binary built at target\debug\kenji.exe (or release)

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$bin = Join-Path $root "target\debug\kenji.exe"
if (-not (Test-Path $bin)) { $bin = Join-Path $root "server\target\debug\kenji.exe" }
if (-not (Test-Path $bin)) { $bin = Join-Path $root "target\release\kenji.exe" }
if (-not (Test-Path $bin)) { Write-Host "kenji.exe not found. Build first." -ForegroundColor Red; exit 1 }

$data = Join-Path $root ".data-smoke"
if (Test-Path $data) { Remove-Item $data -Recurse -Force }
New-Item -ItemType Directory -Path $data | Out-Null

Write-Host "== setup =="
& $bin setup --data-dir $data --username admin --password test123456
if ($LASTEXITCODE -ne 0) { Write-Host "SETUP FAILED" -ForegroundColor Red; exit 1 }

Write-Host "== serve (background) =="
$serverArgs = "serve --data-dir `"$data`""
$server = Start-Process -FilePath $bin -ArgumentList $serverArgs -PassThru -RedirectStandardOutput (Join-Path $data "server.out.log") -RedirectStandardError (Join-Path $data "server.err.log")
Start-Sleep -Seconds 4

$base = "http://127.0.0.1:8787"
$pass = 0; $fail = 0
function Check($name, $cond) {
  if ($cond) { $script:pass++; Write-Host "  PASS $name" -ForegroundColor Green }
  else { $script:fail++; Write-Host "  FAIL $name" -ForegroundColor Red }
}

try {
  $h = Invoke-RestMethod "$base/api/health"
  Check "health" ($h.ok -eq $true)

  # bad login
  try { Invoke-RestMethod -Method Post "$base/api/auth/login" -Body (@{username="admin";password="wrong"} | ConvertTo-Json) -ContentType "application/json" | Out-Null; Check "bad-login-rejected" $false }
  catch { Check "bad-login-rejected" ($true) }

  # login
  $login = Invoke-RestMethod -Method Post "$base/api/auth/login" -Body (@{username="admin";password="test123456"} | ConvertTo-Json) -ContentType "application/json"
  $token = $login.token
  Check "login" ($null -ne $token -and $login.role -eq "admin")
  $H = @{ Authorization = "Bearer $token" }

  $me = Invoke-RestMethod "$base/api/auth/me" -Headers $H
  Check "me" ($me.username -eq "admin")

  # overview
  $ov = Invoke-RestMethod "$base/api/system/overview" -Headers $H
  Check "overview" ($null -ne $ov.cpu -and $null -ne $ov.jobs_summary)

  # create a job that prints numbers for ~5 seconds
  $counterCmd = '1..10 | ForEach-Object { Write-Output "count=$_"; Start-Sleep -Milliseconds 600 }'
  $jobBody = @{ name = "smoke-counter"; command = "powershell.exe"; args = @("-NoProfile","-Command",$counterCmd) } | ConvertTo-Json
  $job = Invoke-RestMethod -Method Post "$base/api/jobs" -Headers $H -ContentType "application/json" -Body $jobBody
  Check "job-create" ($null -ne $job.id)
  $jobId = $job.id

  Start-Sleep -Seconds 2
  $jobs = Invoke-RestMethod "$base/api/jobs" -Headers $H
  $j = $jobs.jobs | Where-Object { $_.id -eq $jobId }
  Check "job-running" ($j.status -eq "running")
  Check "job-pid" ($null -ne $j.pid)

  $logs = Invoke-RestMethod "$base/api/jobs/$jobId/logs" -Headers $H
  Check "job-logs" ($logs.content -match "count=")

  # stop
  $stopped = Invoke-RestMethod -Method Post "$base/api/jobs/$jobId/stop" -Headers $H
  Start-Sleep -Seconds 2
  $j = (Invoke-RestMethod "$base/api/jobs" -Headers $H).jobs | Where-Object { $_.id -eq $jobId }
  Check "job-stopped" ($j.status -eq "stopped")

  # restart (start again)
  $null = Invoke-RestMethod -Method Post "$base/api/jobs/$jobId/start" -Headers $H
  Start-Sleep -Seconds 1
  $j = (Invoke-RestMethod "$base/api/jobs" -Headers $H).jobs | Where-Object { $_.id -eq $jobId }
  Check "job-restart" ($j.status -eq "running")
  $null = Invoke-RestMethod -Method Post "$base/api/jobs/$jobId/stop" -Headers $H
  Start-Sleep -Seconds 2

  # delete
  $null = Invoke-RestMethod -Method Delete "$base/api/jobs/$jobId" -Headers $H
  $j = (Invoke-RestMethod "$base/api/jobs" -Headers $H).jobs | Where-Object { $_.id -eq $jobId }
  Check "job-deleted" ($null -eq $j)
  # files
  $null = Invoke-RestMethod -Method Post "$base/api/files/mkdir" -Headers $H -ContentType "application/json" -Body (@{path="/smoke"} | ConvertTo-Json)
  $tmp = Join-Path $env:TEMP "kenji-up.txt"
  "hello kenji" | Out-File -Encoding ascii $tmp
  # multipart upload via curl.exe (PS5.1 lacks -Form)
  & curl.exe -s -X POST "$base/api/files/upload?path=/smoke" -H "Authorization: Bearer $token" -F "file=@$tmp" | Out-Null
  if ($LASTEXITCODE -ne 0) { Check "file-upload+list" $false }
  $fl = Invoke-RestMethod "$base/api/files/list?path=/smoke" -Headers $H
  Check "file-upload+list" (@($fl.entries | Where-Object { $_.name -eq "kenji-up.txt" }).Count -eq 1)

  $dl = "$env:TEMP\kenji-down.txt"
  Invoke-WebRequest "$base/api/files/download?path=/smoke/kenji-up.txt&token=$token" -OutFile $dl | Out-Null
  Check "file-download" ((Get-Content $dl) -join "" -match "hello kenji")

  # traversal rejected
  try { Invoke-RestMethod "$base/api/files/list?path=../" -Headers $H | Out-Null; Check "traversal-rejected" $false }
  catch { Check "traversal-rejected" ($true) }

  # api keys
  $key = Invoke-RestMethod -Method Post "$base/api/auth/keys" -Headers $H -ContentType "application/json" -Body (@{name="smoke";role="viewer"} | ConvertTo-Json)
  Check "key-created" ($key.key -like "kjk_*")
  $HV = @{ Authorization = "Bearer $($key.key)" }
  $okViewer = $true
  try { Invoke-RestMethod -Method Post "$base/api/files/mkdir" -Headers $HV -ContentType "application/json" -Body (@{path="/nope"} | ConvertTo-Json) | Out-Null; $okViewer = $false } catch {}
  Check "key-role-enforced" ($okViewer)
  $keys = Invoke-RestMethod "$base/api/auth/keys" -Headers $H
  Check "key-listed" (@($keys.keys | Where-Object { $_.name -eq "smoke" }).Count -ge 1)
  $null = Invoke-RestMethod -Method Delete "$base/api/auth/keys/$($key.id)" -Headers $H
  Check "key-deleted" $true

  # audit
  $aud = Invoke-RestMethod "$base/api/audit?limit=50" -Headers $H
  Check "audit-entries" ($aud.entries.Count -ge 5)

  # settings
  $st = Invoke-RestMethod "$base/api/settings" -Headers $H
  Check "settings" ($null -ne $st.tunnel)
}
finally {
  Write-Host "`nStopping server..."
  try { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue } catch {}
  Start-Sleep -Seconds 1
}

Write-Host "`n==== RESULT: $pass passed, $fail failed ===="
exit $(if ($fail -gt 0) { 1 } else { 0 })
