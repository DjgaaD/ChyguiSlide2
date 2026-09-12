@echo off
chcp 65001 >nul
setlocal EnableDelayedExpansion
cd /d "%~dp0"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

rem Справка: build_installer.bat -h
if "%~1"=="-h" goto :usage
if "%~1"=="--help" goto :usage
if "%~1"=="/?" goto :usage

where cargo >nul 2>&1
if errorlevel 1 (
    echo [ОШИБКА] Не найден Cargo.
    echo Установите Rust с https://rustup.rs и запустите сборку снова.
    echo Ожидаемый путь: %USERPROFILE%\.cargo\bin\cargo.exe
    echo.
    pause
    exit /b 1
)

rem Версию можно передать аргументом (build_installer.bat 0.2.0) или
rem переменной окружения APP_VERSION. Если ничего не указано — спросим.
set "VERSION=%~1"
if not defined VERSION set "VERSION=%APP_VERSION%"

if defined VERSION goto :build

for /f "usebackq delims=" %%V in (`powershell -NoProfile -ExecutionPolicy Bypass -File "set_app_version.ps1"`) do set "CURRENT=%%V"
if not defined CURRENT set "CURRENT=0.0.0"
echo Текущая версия приложения: !CURRENT!
echo.
set /p "VERSION=Новая версия (Enter — оставить !CURRENT!): "
if not defined VERSION (
    set "VERSION=!CURRENT!"
    echo Версия оставлена прежней: !CURRENT!
)

:build
echo ========================================
echo ChyguiSlide: сборка установщика
echo ========================================
echo.
echo [1/3] Запись версии !VERSION! в файлы проекта...
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "set_app_version.ps1" -Version "!VERSION!"
if errorlevel 1 (
    echo.
    echo [ОШИБКА] Версия не записана — сборка отменена.
    echo.
    pause
    exit /b 1
)

echo.
echo [2/3] Сборка интерфейса и приложения (это может занять несколько минут)...
echo.
call npm run tauri build
if errorlevel 1 (
    echo.
    echo [ОШИБКА] Сборка не удалась. Смотрите вывод выше.
    echo.
    pause
    exit /b 1
)

set "INSTALLER="
for /f "delims=" %%F in ('dir /b /o-d "src-tauri\target\release\bundle\nsis\ChyguiSlide_*-setup.exe" 2^>nul') do if not defined INSTALLER set "INSTALLER=%%F"

echo.
echo [3/3] Готово.
if defined INSTALLER (
    echo Установщик: src-tauri\target\release\bundle\nsis\!INSTALLER!
) else (
    echo Установщик не найден — проверьте вывод сборки выше.
)
echo.
echo Дальше: powershell -ExecutionPolicy Bypass -File prepare_release.ps1
echo Скрипт упакует установщик, посчитает SHA-256 и напечатает блок для README.
echo.
pause
exit /b 0

:usage
echo Использование: build_installer.bat [версия]
echo.
echo   build_installer.bat          спросит новую версию
echo   build_installer.bat 0.2.0    соберёт установщик версии 0.2.0
echo.
echo Версию можно задать и переменной окружения APP_VERSION.
echo Номер записывается в src-tauri\tauri.conf.json, package.json и
echo src-tauri\Cargo.toml — версия в «О нас» берётся из tauri.conf.json.
echo.
pause
exit /b 0
