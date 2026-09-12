<#
    prepare_release.ps1 — готовит файлы для релиза ChyguiSlide на GitHub.

    Что делает:
      1. находит самый свежий установщик в src-tauri\target\release\bundle\nsis;
      2. копирует его в папку release\ — GitHub принимает .exe в ассетах релиза
         (до 2 ГБ на файл), поэтому упаковка не нужна;
      3. считает SHA-256 установщика — это значение попадает в манифест и
         проверяется при обновлении и при установке через install.ps1;
      4. печатает версию, файл и контрольную сумму.

    Ключ -Pack дополнительно упаковывает установщик в .zip и, если архив больше
    PartSizeMb, режет его на gzip-части. Это нужно только для хостингов с
    лимитом на размер файла; для GitHub не требуется.

    Запуск:
      powershell -ExecutionPolicy Bypass -File prepare_release.ps1
      powershell -ExecutionPolicy Bypass -File prepare_release.ps1 -Pack

    Код возврата: 0 — успех, 1 — ошибка.
#>
[CmdletBinding()]
param(
    [int]$PartSizeMb = 95,
    [switch]$Pack,
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem | Out-Null

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$nsisDir = Join-Path $root 'src-tauri\target\release\bundle\nsis'
if (-not (Test-Path $nsisDir)) {
    throw "Не найден каталог установщика: $nsisDir`nСначала выполните build_installer.bat."
}

$installer = Get-ChildItem -Path $nsisDir -Filter 'ChyguiSlide_*-setup.exe' -File |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $installer) {
    throw "В каталоге $nsisDir нет файлов ChyguiSlide_*-setup.exe."
}

if ($installer.BaseName -match '^ChyguiSlide_(?<version>[0-9]+(\.[0-9]+)*)_x64-setup$') {
    $version = $Matches['version']
} else {
    throw "Не удалось определить версию из имени $($installer.Name). Ожидается ChyguiSlide_<версия>_x64-setup.exe."
}

$releaseDir = Join-Path $root 'release'
if (Test-Path $releaseDir) {
    Get-ChildItem -Path $releaseDir -Filter 'ChyguiSlide_*' -File | Remove-Item -Force
} else {
    New-Item -ItemType Directory -Path $releaseDir | Out-Null
}

$target = Join-Path $releaseDir $installer.Name
Copy-Item -LiteralPath $installer.FullName -Destination $target -Force

$sizeBytes = (Get-Item -LiteralPath $target).Length
$sha256 = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()

Write-Host "[1/2] Установщик: $($installer.Name)"
Write-Host ('      размер: {0:N1} МБ' -f ($sizeBytes / 1MB))
Write-Host "      sha256: $sha256"
Write-Host "      лежит в: release\$($installer.Name)"

$files = @($installer.Name)

if ($Pack) {
    $zipName = "ChyguiSlide_${version}_x64-setup.zip"
    $zipPath = Join-Path $releaseDir $zipName
    Write-Host "[2/2] Упаковка в архив $zipName (для хостингов с лимитом на файл)"
    $archive = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $archive, $target, $installer.Name,
            [System.IO.Compression.CompressionLevel]::NoCompression)
    } finally {
        $archive.Dispose()
    }

    $zipSize = (Get-Item $zipPath).Length
    $sha256 = (Get-FileHash -Path $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    Write-Host ('      размер архива: {0:N1} МБ' -f ($zipSize / 1MB))
    Write-Host "      sha256 архива: $sha256"

    $partSize = [int64]$PartSizeMb * 1MB
    if ($zipSize -le $partSize) {
        Write-Host '      архив укладывается в лимит — части не нужны'
        $files = @($zipName)
    } else {
        $parts = [int][Math]::Ceiling($zipSize / $partSize)
        Write-Host "      архив больше $PartSizeMb МБ — режем на $parts gzip-частей"
        $files = @()
        # Имя потока не должно совпадать со встроенными переменными PowerShell
        # ($input, $args и т. п.) — правило PSAvoidAssignmentToAutomaticVariable.
        $zipFileStream = [System.IO.File]::OpenRead($zipPath)
        try {
            $buffer = [byte[]]::new(1MB)
            $index = 0
            while ($true) {
                $index++
                $partName = "ChyguiSlide_${version}_x64-setup.part$index.gz"
                $partPath = Join-Path $releaseDir $partName
                $written = [int64]0
                $partStream = [System.IO.File]::Create($partPath)
                $gzip = [System.IO.Compression.GzipStream]::new(
                    $partStream, [System.IO.Compression.CompressionMode]::Compress)
                try {
                    while ($written -lt $partSize) {
                        $want = [int][Math]::Min([int64]$buffer.Length, $partSize - $written)
                        $read = $zipFileStream.Read($buffer, 0, $want)
                        if ($read -le 0) { break }
                        $gzip.Write($buffer, 0, $read)
                        $written += $read
                    }
                } finally {
                    $gzip.Dispose()
                }
                if ($written -eq 0) {
                    Remove-Item $partPath -Force
                    break
                }
                $files += $partName
                Write-Host ('      {0} — {1:N1} МБ' -f $partName, ((Get-Item $partPath).Length / 1MB))
                if ($written -lt $partSize) { break }
            }
        } finally {
            $zipFileStream.Dispose()
        }
    }
} else {
    Write-Host '[2/2] Упаковка не нужна: GitHub принимает .exe в ассетах релиза'
}

$tag = "v$version"

if (-not $Quiet) {
    Write-Host ''
    Write-Host 'Файлы для загрузки в релиз:'
    foreach ($file in $files) { Write-Host ("  release\{0}" -f $file) }
    Write-Host ''
    Write-Host 'Дальше: powershell -ExecutionPolicy Bypass -File publish_github.ps1'
    Write-Host "Скрипт создаст релиз $tag, загрузит эти файлы, подставит в Update.md"
    Write-Host 'блок манифеста с настоящими ссылками и опубликует Update.md,'
    Write-Host 'README.md и install.ps1 в репозитории.'
}
