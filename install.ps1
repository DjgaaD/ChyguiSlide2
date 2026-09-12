<#
    install.ps1 — установка ChyguiSlide на новый компьютер.

    Зачем нужен: скрипт читает манифест обновления из Update.md, скачивает
    файлы релиза, при необходимости распаковывает и склеивает их, сверяет
    SHA-256 и запускает установщик — то же самое делает приложение при
    обновлении. На GitHub установщик лежит в релизе одним файлом `.exe`, так что
    обычно скачивается ровно один файл.

    Запуск одной строкой (ничего скачивать вручную не нужно):

      & ([scriptblock]::Create((irm 'https://raw.githubusercontent.com/DjgaaD/ChyguiSlide2/master/install.ps1').TrimStart([char]0xFEFF)))

    TrimStart убирает BOM в начале файла: в строке скрипта он превращается в
    мусорные символы и PowerShell не разбирает первую строку. Сам файл хранится
    с BOM, иначе PowerShell 5.1 читает кириллицу как ANSI.

    Или файлом: скачайте install.ps1 и выполните

      powershell -ExecutionPolicy Bypass -File install.ps1

    Ключи:
      -ManifestUrl <адрес|путь> — откуда брать манифест (по умолчанию Update.md
                                  из репозитория; можно указать путь к файлу);
      -PartsDir <папка>         — взять части из папки, ничего не скачивать;
      -WorkDir <папка>          — рабочая папка (по умолчанию TEMP);
      -SkipInstall              — собрать и распаковать, установщик не запускать;
      -Silent                   — установить без окон (/S /UPDATE /R);
      -KeepFiles                — не удалять скачанные файлы;
      -Yes                      — не спрашивать подтверждение.

    Код возврата: 0 — успех, 1 — отмена или ошибка.
#>
[CmdletBinding()]
param(
    [string]$ManifestUrl = 'https://raw.githubusercontent.com/DjgaaD/ChyguiSlide2/master/Update.md',
    [string]$PartsDir,
    [string]$WorkDir = (Join-Path $env:TEMP 'ChyguiSlide-setup'),
    [switch]$SkipInstall,
    [switch]$Silent,
    [switch]$KeepFiles,
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Add-Type -AssemblyName System.IO.Compression | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem | Out-Null

$marker = '<!-- CHYGUISLIDE-UPDATE -->'
$attempts = 3

# Текст манифеста: с диска или из сети. Иногда вместо файла приходит страница
# (веб-интерфейс, заглушка прокси) — такой ответ повторяем.
function Get-ManifestText {
    param([Parameter(Mandatory)][string]$Url)
    if ($Url -notmatch '^https?://') {
        if (-not (Test-Path -LiteralPath $Url)) { throw "Файл манифеста не найден: $Url" }
        return [System.IO.File]::ReadAllText((Resolve-Path -LiteralPath $Url).Path)
    }
    $note = 'нет ответа'
    for ($attempt = 1; $attempt -le $attempts; $attempt++) {
        try {
            $response = Invoke-WebRequest -UseBasicParsing -Uri $Url -TimeoutSec 60
            $text = [string]$response.Content
        } catch {
            $note = $_.Exception.Message
            Write-Host "      попытка $attempt не удалась, повторяю"
            Start-Sleep -Seconds 3
            continue
        }
        if ($text -match '<!DOCTYPE|<html') {
            $note = 'пришла страница вместо файла'
            Write-Host "      $note, повторяю"
            Start-Sleep -Seconds 3
            continue
        }
        if (-not $text.Contains($marker)) {
            $note = "в ответе нет маркера $marker"
            Write-Host "      $note, повторяю"
            Start-Sleep -Seconds 3
            continue
        }
        return $text
    }
    throw "Не удалось получить описание обновления $Url — $note."
}

# Блок манифеста: маркер, ограда ```json и JSON внутри — тот же формат,
# что разбирает приложение (src-tauri/src/updater.rs).
function Get-Manifest {
    param([Parameter(Mandatory)][string]$Text)
    $match = [regex]::Match($Text, [regex]::Escape($marker) + '\s*```json\s*(?<json>.*?)\s*```',
        [System.Text.RegularExpressions.RegexOptions]::Singleline)
    if (-not $match.Success) {
        throw "В файле нет блока манифеста с маркером $marker."
    }
    $manifest = $match.Groups['json'].Value | ConvertFrom-Json
    if (-not $manifest.version) { throw 'В манифесте не указана версия.' }
    return $manifest
}

# Первые два байта файла: 'PK' — zip, 'MZ' — программа, 1F 8B — gzip.
function Get-Magic {
    param([Parameter(Mandatory)][string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        $head = New-Object byte[] 2
        $read = $stream.Read($head, 0, 2)
    } finally { $stream.Dispose() }
    if ($read -lt 2) { return @(0, 0) }
    return @($head[0], $head[1])
}

# --- 1. Манифест --------------------------------------------------------------

Write-Host '[1/5] Чтение описания последней версии'
$manifestText = Get-ManifestText -Url $ManifestUrl
$manifest = Get-Manifest -Text $manifestText

$urls = @()
if ($manifest.parts) { $urls = @($manifest.parts) }
elseif ($manifest.downloadUrl) { $urls = @($manifest.downloadUrl) }
if ($urls.Count -eq 0) {
    throw "В манифесте версии $($manifest.version) нет ни parts, ни downloadUrl."
}

Write-Host ''
Write-Host "ChyguiSlide $($manifest.version)"
if ($manifest.publishedAt) { Write-Host "Опубликовано: $($manifest.publishedAt)" }
if ($manifest.sizeBytes) {
    Write-Host ("Размер загрузки: {0:N1} МБ в {1} файл(ах)" -f ([double]$manifest.sizeBytes / 1MB), $urls.Count)
}
if ($manifest.notes) {
    Write-Host 'Что нового:'
    foreach ($line in ([string]$manifest.notes -split "`n")) {
        $trimmed = $line.TrimEnd("`r").TrimEnd()
        if ($trimmed) { Write-Host "  $trimmed" }
    }
}
Write-Host ''

if (-not $Yes) {
    $answer = Read-Host 'Продолжить установку? (Enter — да, N — отмена)'
    if ($answer -match '^(n|no|нет|н)$') {
        Write-Host 'Отменено.'
        # Файл запускают через -File (там нужен код возврата), а строку — прямо
        # в сессии PowerShell: в этом случае просто заканчиваем, не закрывая окно.
        if ($PSCommandPath) { exit 1 }
        return
    }
}

# --- 2. Скачивание частей ----------------------------------------------------

Write-Host '[2/5] Файлы обновления'
if (-not (Test-Path -LiteralPath $WorkDir)) {
    New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null
}
# Убираем только свои прежние файлы: папку мог задать пользователь.
foreach ($stale in @('update.zip', 'ChyguiSlide-setup.exe', 'unpacked')) {
    $stalePath = Join-Path $WorkDir $stale
    if (Test-Path -LiteralPath $stalePath) { Remove-Item -LiteralPath $stalePath -Recurse -Force }
}

$partPaths = @()
if ($PartsDir) {
    if (-not (Test-Path -LiteralPath $PartsDir)) { throw "Папка с частями не найдена: $PartsDir" }
    $found = @(Get-ChildItem -LiteralPath $PartsDir -File | Sort-Object Name)
    $gzipParts = @($found | Where-Object { $_.Name -like '*.gz' })
    if ($gzipParts.Count -gt 0) { $found = $gzipParts }
    if ($found.Count -ne $urls.Count) {
        throw "В папке $PartsDir файлов $($found.Count), а в манифесте частей $($urls.Count)."
    }
    foreach ($file in $found) {
        Write-Host ("      {0} — {1:N1} МБ (из папки)" -f $file.Name, ($file.Length / 1MB))
        $partPaths += $file.FullName
    }
} else {
    for ($index = 0; $index -lt $urls.Count; $index++) {
        $path = Join-Path $WorkDir ('part{0}.download' -f ($index + 1))
        Write-Host ("      часть {0} из {1}" -f ($index + 1), $urls.Count)
        for ($attempt = 1; $attempt -le $attempts; $attempt++) {
            try {
                Invoke-WebRequest -UseBasicParsing -Uri $urls[$index] -OutFile $path -TimeoutSec 1800
                break
            } catch {
                if ($attempt -eq $attempts) {
                    throw "Файл не скачался: $($urls[$index]) — $($_.Exception.Message)"
                }
                Write-Host "      попытка $attempt не удалась, повторяю"
                Start-Sleep -Seconds 5
            }
        }
        Write-Host ("      {0:N1} МБ" -f ((Get-Item -LiteralPath $path).Length / 1MB))
        $partPaths += $path
    }
}

# --- 3. Склейка и проверка ---------------------------------------------------

Write-Host '[3/5] Сборка архива'
$assembled = Join-Path $WorkDir 'update.zip'
$zipStream = [System.IO.File]::Create($assembled)
try {
    foreach ($part in $partPaths) {
        $partStream = [System.IO.File]::OpenRead($part)
        try {
            $magic = Get-Magic -Path $part
            if ($magic[0] -eq 0x1F -and $magic[1] -eq 0x8B) {
                $gzip = New-Object System.IO.Compression.GzipStream(
                    $partStream, [System.IO.Compression.CompressionMode]::Decompress)
                try { $gzip.CopyTo($zipStream) } finally { $gzip.Dispose() }
            } else {
                $partStream.CopyTo($zipStream)
            }
        } finally { $partStream.Dispose() }
    }
} finally { $zipStream.Dispose() }

$assembledSize = (Get-Item -LiteralPath $assembled).Length
Write-Host ("      собрано {0:N1} МБ" -f ($assembledSize / 1MB))
if ($manifest.sizeBytes -and [int64]$manifest.sizeBytes -ne $assembledSize) {
    Write-Host "      внимание: размер не совпал с манифестом ($($manifest.sizeBytes) байт)"
}
if ($manifest.sha256) {
    $hash = (Get-FileHash -LiteralPath $assembled -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($hash -ne ([string]$manifest.sha256).ToLowerInvariant()) {
        if (-not $KeepFiles) { Remove-Item -LiteralPath $WorkDir -Recurse -Force -ErrorAction SilentlyContinue }
        throw "Контрольная сумма не совпала: собрано $hash, в манифесте $($manifest.sha256)." +
            ' Файлы удалены — запустите скрипт ещё раз.'
    }
    Write-Host "      SHA-256 совпал: $hash"
} else {
    Write-Host '      в манифесте нет sha256 — проверка пропущена'
}

# --- 4. Распаковка -----------------------------------------------------------

Write-Host '[4/5] Распаковка'
$magic = Get-Magic -Path $assembled
$installer = $null
if ($magic[0] -eq 0x50 -and $magic[1] -eq 0x4B) {
    $unpacked = Join-Path $WorkDir 'unpacked'
    [System.IO.Compression.ZipFile]::ExtractToDirectory($assembled, $unpacked)
    $installer = Get-ChildItem -LiteralPath $unpacked -Recurse -File -Filter '*-setup.exe' |
        Select-Object -First 1
    if (-not $installer) {
        $installer = Get-ChildItem -LiteralPath $unpacked -Recurse -File -Filter '*.exe' |
            Select-Object -First 1
    }
    Remove-Item -LiteralPath $assembled -Force
} elseif ($magic[0] -eq 0x4D -and $magic[1] -eq 0x5A) {
    $installerPath = Join-Path $WorkDir 'ChyguiSlide-setup.exe'
    Move-Item -LiteralPath $assembled -Destination $installerPath -Force
    $installer = Get-Item -LiteralPath $installerPath
} else {
    throw ("Файл обновления не похож ни на архив, ни на программу " +
        '(первые байты: {0:X2} {1:X2}).' -f $magic[0], $magic[1])
}
if (-not $installer) { throw 'В архиве не найден установщик.' }
Write-Host ("      установщик: {0} — {1:N1} МБ" -f $installer.Name, ($installer.Length / 1MB))

# --- 5. Установка ------------------------------------------------------------

Write-Host '[5/5] Установка'
if ($SkipInstall) {
    Write-Host "      запуск пропущен (-SkipInstall): $($installer.FullName)"
} else {
    if ($Silent) {
        $arguments = @('/S', '/UPDATE', '/R')
        Write-Host '      установка без окон (/S /UPDATE /R)'
    } else {
        $arguments = @()
        Write-Host '      откроется окно установщика'
    }
    Start-Process -FilePath $installer.FullName -ArgumentList $arguments -Wait
    Write-Host '      установщик завершил работу'
}

if (-not $KeepFiles) {
    Remove-Item -LiteralPath $WorkDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host '      временные файлы удалены'
}
Write-Host ''
Write-Host "Готово: ChyguiSlide $($manifest.version)."
if ($PSCommandPath) { exit 0 }
