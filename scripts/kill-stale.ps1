# Stops leftover Tauri debug processes from THIS project only.
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -ErrorAction SilentlyContinue
if (-not $root) { $root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path }

$markers = @(
  (Join-Path $root "src-tauri\target\debug\chyguislide.exe"),
  (Join-Path $root "src-tauri\target\debug\chyguislide.pdb")
)

Get-Process -Name "chyguislide" -ErrorAction SilentlyContinue | ForEach-Object {
  $path = $_.Path
  if ($path -and ($path -like "*ChyguiSlide 2*" -or $path -like "*\src-tauri\target\debug\chyguislide.exe")) {
    Write-Host "Stopping leftover $($_.ProcessName).exe (PID $($_.Id))..."
    Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
  }
}
