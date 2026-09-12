@echo off
setlocal EnableExtensions
cd /d "%~dp0"

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "VITE_PORT=1420"
set "HMR_PORT=1421"

where cargo >nul 2>&1
if errorlevel 1 (
  echo [ERROR] cargo not found. Install Rust from https://rustup.rs
  echo Expected path: %USERPROFILE%\.cargo\bin\cargo.exe
  pause
  exit /b 1
)

where npm >nul 2>&1
if errorlevel 1 (
  echo [ERROR] npm not found. Install Node.js and reopen the terminal.
  pause
  exit /b 1
)

echo Checking ports %VITE_PORT%/%HMR_PORT%...
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\free-port.ps1" -Port %VITE_PORT%
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\free-port.ps1" -Port %HMR_PORT%
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\kill-stale.ps1"

if not exist "node_modules\" (
  echo Installing npm dependencies...
  call npm install
  if errorlevel 1 (
    echo [ERROR] npm install failed
    pause
    exit /b 1
  )
)

echo Starting ChyguiSlide...
call npm run tauri dev
set "EXIT_CODE=%ERRORLEVEL%"

if not "%EXIT_CODE%"=="0" (
  echo.
  echo [ERROR] App exited with code %EXIT_CODE%
  pause
)

endlocal
exit /b %EXIT_CODE%
