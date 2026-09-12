param(
  [Parameter(Mandatory = $true)]
  [int]$Port
)

# Fast path: parse netstat (Get-NetTCPConnection is often very slow / hangs on Windows).
function Get-ListenPids([int]$PortNumber) {
  $pattern = ":$PortNumber\s"
  $pids = New-Object System.Collections.Generic.HashSet[int]
  $lines = & netstat.exe -ano -p tcp 2>$null
  foreach ($line in $lines) {
    if ($line -notmatch "LISTENING") { continue }
    if ($line -notmatch $pattern) { continue }
    $parts = ($line -split "\s+") | Where-Object { $_ -ne "" }
    if ($parts.Count -lt 5) { continue }
    $procId = 0
    if ([int]::TryParse($parts[-1], [ref]$procId) -and $procId -gt 0) {
      [void]$pids.Add($procId)
    }
  }
  return @($pids)
}

$pids = Get-ListenPids -PortNumber $Port
if ($pids.Count -eq 0) {
  exit 0
}

foreach ($procId in $pids) {
  Write-Host "Freeing port $Port (PID $procId)..."
  & taskkill.exe /F /PID $procId 2>$null | Out-Null
}

Start-Sleep -Milliseconds 400
exit 0
