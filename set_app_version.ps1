<#
    set_app_version.ps1 — записывает версию приложения во все файлы, где она
    указана, чтобы номера не разошлись:

      src-tauri\tauri.conf.json — версия сборки: её показывает «О нас» и по ней
                                  сравнивается обновление;
      package.json              — версия интерфейса;
      src-tauri\Cargo.toml      — версия раздела [package].

    Запуск:
      powershell -ExecutionPolicy Bypass -File set_app_version.ps1
        печатает текущую версию одной строкой (так её читает build_installer.bat);
      powershell -ExecutionPolicy Bypass -File set_app_version.ps1 -Version 0.2.0
        записывает версию 0.2.0 во все три файла.

    Код возврата: 0 — успех, 1 — ошибка (неверный номер или файл не найден).
#>
[CmdletBinding()]
param(
    [string]$Version
)

$ErrorActionPreference = 'Stop'
$utf8NoBom = New-Object System.Text.UTF8Encoding $false

$root = Split-Path -Parent $MyInvocation.MyCommand.Path

# Одинаковая форма записи версии во всех файлах: <префикс>"<версия>"<суффикс>.
# Раздел нужен для Cargo.toml: там `version = "…"` встречается и у зависимостей.
$targets = @(
    [pscustomobject]@{
        Path    = 'src-tauri\tauri.conf.json'
        Pattern = '(?m)^(\s*"version"\s*:\s*")([^"]*)(")'
        Section = ''
    },
    [pscustomobject]@{
        Path    = 'package.json'
        Pattern = '(?m)^(\s*"version"\s*:\s*")([^"]*)(")'
        Section = ''
    },
    [pscustomobject]@{
        Path    = 'src-tauri\Cargo.toml'
        Pattern = '(?m)^(version\s*=\s*")([^"]*)(")'
        Section = '[package]'
    }
)

# Делит текст файла на «до раздела» и «с раздела»: версию ищем только в нужном
# разделе, чтобы не задеть `version` у зависимостей.
function Split-AtSection([string]$text, [string]$section) {
    if (-not $section) {
        return @('', $text)
    }
    $start = $text.IndexOf($section)
    if ($start -lt 0) {
        return $null
    }
    return @($text.Substring(0, $start), $text.Substring($start))
}

function Read-Target($target) {
    $path = Join-Path $root $target.Path
    if (-not (Test-Path $path)) {
        throw "не найден файл $($target.Path)"
    }
    $parts = Split-AtSection ([System.IO.File]::ReadAllText($path)) $target.Section
    if (-not $parts) {
        throw "в $($target.Path) не найден раздел $($target.Section)"
    }
    return @($path, $parts[0], $parts[1])
}

function Get-TargetVersion($target) {
    $parts = Read-Target $target
    $match = [regex]::Match($parts[2], $target.Pattern)
    if (-not $match.Success) {
        throw "в $($target.Path) не найдена строка с версией"
    }
    return $match.Groups[2].Value
}

function Set-TargetVersion($target, [string]$version) {
    $parts = Read-Target $target
    $regex = [regex]::new($target.Pattern)
    if (-not $regex.IsMatch($parts[2])) {
        throw "в $($target.Path) не найдена строка с версией"
    }
    $body = $regex.Replace($parts[2], '${1}' + $version + '${3}', 1)
    [System.IO.File]::WriteAllText($parts[0], $parts[1] + $body, $utf8NoBom)
}

$current = Get-TargetVersion $targets[0]

if (-not $Version) {
    Write-Output $current
    exit 0
}

$Version = $Version.Trim()
if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z][0-9A-Za-z.\-]*)?$') {
    Write-Host "[ОШИБКА] Неверный номер версии: `"$Version`""
    Write-Host 'Нужен вид 1.2.3 или 1.2.3-beta.1, например 0.2.0.'
    exit 1
}

if ($Version -eq $current) {
    Write-Host "Версия не изменилась: $current"
    exit 0
}

if ($current -match '^\d+(\.\d+)*$' -and $Version -match '^\d+(\.\d+)*$') {
    if ([version]$Version -le [version]$current) {
        Write-Host "[ВНИМАНИЕ] Версия $Version не больше текущей ${current}:"
        Write-Host 'обновление появится у пользователей только при большем номере.'
    }
}

foreach ($target in $targets) {
    Set-TargetVersion $target $Version
}

Write-Host "Версия приложения: $current -> $Version"
foreach ($target in $targets) {
    Write-Host ("  {0} — {1}" -f $target.Path, $Version)
}
exit 0
