<#
    publish_gitverse.ps1 — публикует релиз ChyguiSlide и описание обновления на GitVerse.

    Что делает:
      1. берёт версию и файлы из папки release/ (их готовит prepare_release.ps1);
      2. создаёт релиз с тегом v<версия> (или берёт уже созданный);
      3. прикрепляет файлы из release/ к релизу и запоминает их адреса;
      4. собирает блок манифеста и подставляет его в Update.md;
      5. публикует Update.md, README.md и install.ps1;
      6. проверяет, что Update.md, README.md, install.ps1 и файлы релиза
         скачиваются анонимно.

    Описание обновления живёт в отдельном файле Update.md, а README.md остаётся
    описанием программы. Update.md публикуется первым: с этого момента программа
    видит новую версию, а README можно менять независимо от неё.

    Адреса файлов в манифесте — те, что GitVerse выдаёт при загрузке ассета
    (https://gitverse.ru/api/attachments/<идентификатор>): адрес вида
    /releases/download/<тег>/<файл> на GitVerse не работает и отдаёт 404.

    Токен нужен с правом «Запись» на репозиторий: GitVerse → Профиль →
    Управление токенами. Передаётся параметром -Token или переменной окружения
    GITVERSE_TOKEN; в файлы проекта токен не записывается.

    Запуск:
      powershell -ExecutionPolicy Bypass -File publish_gitverse.ps1 -DryRun
        показать версию, файлы и SHA-256, без обращений к сети;
      $env:GITVERSE_TOKEN = '<токен>'
      powershell -ExecutionPolicy Bypass -File publish_gitverse.ps1

    Ключи: -VerifyDownload — дополнительно скачать файлы релиза анонимно,
    склеить части и сверить SHA-256; -SkipDocs — не публиковать Update.md
    и README.md.

    Код возврата: 0 — успех, 1 — ошибка.
#>
[CmdletBinding()]
param(
    [string]$Token = $env:GITVERSE_TOKEN,
    [string]$Owner = 'DjgaaD',
    [string]$Repo = 'ChyguiSlide',
    [string]$Branch = 'master',
    [string]$Version,
    [string]$Notes,
    [switch]$DryRun,
    [switch]$SkipDocs,
    [switch]$VerifyDownload
)

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Add-Type -AssemblyName System.IO.Compression | Out-Null

$apiUrl = 'https://api.gitverse.ru'
$webUrl = 'https://gitverse.ru'
# $webUrl — только хост: адрес репозитория и базовый адрес файлов в ветке
# собираются из него один раз, чтобы ссылки в тексте релиза и в проверках
# скачивания не расходились.
$repoUrl = '{0}/{1}/{2}' -f $webUrl, $Owner, $Repo
$contentBase = '{0}/content/{1}' -f $repoUrl, $Branch
$accept = 'application/vnd.gitverse.object+json;version=1'
$marker = '<!-- CHYGUISLIDE-UPDATE -->'
$utf8NoBom = New-Object System.Text.UTF8Encoding $false

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$releaseDir = Join-Path $root 'release'
$readmePath = Join-Path $root 'README.gitverse.md'
$updatePath = Join-Path $root 'Update.md'
$installPath = Join-Path $root 'install.ps1'
# install.ps1 публикуется вместе с документацией: без него новым пользователям
# нечего запускать, поэтому проверяем файл до обращений к сети.
if (-not $SkipDocs -and -not (Test-Path -LiteralPath $installPath)) {
    throw "Не найден $installPath — он публикуется вместе с Update.md и README.md."
}

# Текст ошибки HTTP-запроса: у GitVerse причина приходит в теле ответа.
function Get-ApiError {
    param([Parameter(Mandatory)]$ErrorRecord)
    $response = $ErrorRecord.Exception.Response
    if ($response) {
        try {
            $reader = New-Object System.IO.StreamReader($response.GetResponseStream())
            return "$([int]$response.StatusCode): $($reader.ReadToEnd())"
        } catch {
            return "$([int]$response.StatusCode): $($ErrorRecord.Exception.Message)"
        }
    }
    return $ErrorRecord.Exception.Message
}

# Запрос к публичному API GitVerse. AllowFailure — вернуть $null вместо ошибки.
function Invoke-GitVerseApi {
    param(
        [Parameter(Mandatory)][string]$Method,
        [Parameter(Mandatory)][string]$Path,
        [string]$Body,
        [switch]$AllowFailure
    )
    $headers = @{ Authorization = "Bearer $Token"; Accept = $accept }
    try {
        if ($Body) {
            return Invoke-RestMethod -Method $Method -Uri "$apiUrl$Path" -Headers $headers `
                -ContentType 'application/json; charset=utf-8' `
                -Body ([System.Text.Encoding]::UTF8.GetBytes($Body))
        }
        return Invoke-RestMethod -Method $Method -Uri "$apiUrl$Path" -Headers $headers
    } catch {
        $message = Get-ApiError $_
        if ($AllowFailure) {
            Write-Host "      $Method $Path — $message"
            return $null
        }
        throw "Запрос $Method $Path не удался — $message"
    }
}

# Публикация файла в репозитории: если содержимое уже такое же, ничего не делает.
# Ответ GitVerse плоский: sha, encoding и content лежат в корне объекта.
function Set-RepoFile {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Content,
        [Parameter(Mandatory)][string]$Message
    )
    $existing = Invoke-GitVerseApi -Method GET -Path "/repos/$Owner/$Repo/contents/${Path}?ref=$Branch" -AllowFailure
    $remoteContent = $null
    if ($existing -and $existing.type -eq 'file' -and $existing.encoding -eq 'base64' -and $existing.content) {
        $remoteContent = [System.Text.Encoding]::UTF8.GetString(
            [Convert]::FromBase64String(($existing.content -replace '\s', '')))
    }
    if ($remoteContent -eq $Content) {
        Write-Host "      $Path уже такой в репозитории — публикация не нужна"
        return $null
    }
    $payload = [ordered]@{
        content = [Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes($Content))
        message = $Message
        branch  = $Branch
    }
    if ($existing -and $existing.sha) {
        $payload['sha'] = $existing.sha
    }
    $result = Invoke-GitVerseApi -Method PUT -Path "/repos/$Owner/$Repo/contents/$Path" -Body ($payload | ConvertTo-Json -Depth 5)
    Write-Host "      $Path опубликован в ветке $Branch"
    if ($result -and $result.commit -and $result.commit.sha) {
        Write-Host "      коммит $($result.commit.sha)"
    }
    return $result
}

# Загрузка ассета: multipart/form-data, поле attachment. Через .NET, чтобы токен
# не попадал ни в аргументы командной строки, ни во временные файлы.
function Send-ReleaseAsset {
    param(
        [Parameter(Mandatory)][string]$ReleaseId,
        [Parameter(Mandatory)][System.IO.FileInfo]$File
    )
    Add-Type -AssemblyName System.Net.Http | Out-Null
    $client = New-Object System.Net.Http.HttpClient
    $client.Timeout = [TimeSpan]::FromHours(2)
    $client.DefaultRequestHeaders.Authorization =
        New-Object System.Net.Http.Headers.AuthenticationHeaderValue('Bearer', $Token)
    $client.DefaultRequestHeaders.Accept.ParseAdd($accept)
    $stream = [System.IO.File]::OpenRead($File.FullName)
    $content = New-Object System.Net.Http.MultipartFormDataContent
    $part = New-Object System.Net.Http.StreamContent($stream)
    $part.Headers.ContentType =
        [System.Net.Http.Headers.MediaTypeHeaderValue]::Parse('application/octet-stream')
    $content.Add($part, 'attachment', $File.Name)
    try {
        $uri = "$apiUrl/repos/$Owner/$Repo/releases/$ReleaseId/assets?name=$($File.Name)"
        $response = $client.PostAsync($uri, $content).GetAwaiter().GetResult()
        $text = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
        if (-not $response.IsSuccessStatusCode) {
            throw "Загрузка $($File.Name) не удалась — $([int]$response.StatusCode): $text"
        }
        return ($text | ConvertFrom-Json)
    } finally {
        $content.Dispose()
        $stream.Dispose()
        $client.Dispose()
    }
}

# Блок манифеста в том же виде, что и в README: отступ в два пробела.
function ConvertTo-ManifestJson {
    param([Parameter(Mandatory)][System.Collections.Specialized.OrderedDictionary]$Manifest)
    $quote = {
        param($value)
        return (ConvertTo-Json -InputObject ([string]$value) -Compress)
    }
    $lines = New-Object System.Collections.Generic.List[string]
    $lines.Add('{')
    $lines.Add("  `"version`": " + (& $quote $Manifest['version']) + ',')
    $lines.Add("  `"publishedAt`": " + (& $quote $Manifest['publishedAt']) + ',')
    $lines.Add("  `"mandatory`": $($Manifest['mandatory'].ToString().ToLowerInvariant()),")
    $lines.Add("  `"notes`": " + (& $quote $Manifest['notes']) + ',')
    if ($Manifest.Contains('downloadUrl')) {
        $lines.Add("  `"downloadUrl`": " + (& $quote $Manifest['downloadUrl']) + ',')
    }
    if ($Manifest.Contains('parts')) {
        $lines.Add('  "parts": [')
        $urls = @($Manifest['parts'])
        for ($index = 0; $index -lt $urls.Count; $index++) {
            $comma = ''
            if ($index -lt $urls.Count - 1) { $comma = ',' }
            $lines.Add('    ' + (& $quote $urls[$index]) + $comma)
        }
        $lines.Add('  ],')
    }
    $lines.Add("  `"sha256`": " + (& $quote $Manifest['sha256']) + ',')
    $lines.Add("  `"sizeBytes`": $($Manifest['sizeBytes'])")
    $lines.Add('}')
    return ($lines -join "`r`n")
}

# --- 1. Версия и файлы релиза ------------------------------------------------

if (-not (Test-Path $releaseDir)) {
    throw "Не найдена папка release\. Сначала выполните prepare_release.ps1."
}

$zips = @(Get-ChildItem -Path $releaseDir -Filter 'ChyguiSlide_*_x64-setup.zip' -File)
$partFiles = @(Get-ChildItem -Path $releaseDir -Filter 'ChyguiSlide_*_x64-setup.part*.gz' -File |
    Sort-Object Name)

if (-not $Version) {
    $source = $null
    if ($zips.Count -gt 0) {
        $source = ($zips | Sort-Object Name -Descending)[0].Name
    } elseif ($partFiles.Count -gt 0) {
        $source = $partFiles[0].Name
    } else {
        throw 'В папке release\ нет файлов релиза. Сначала выполните prepare_release.ps1.'
    }
    if ($source -notmatch '^ChyguiSlide_(?<version>[0-9]+(\.[0-9]+)*)_x64-setup') {
        throw "Не удалось определить версию из имени $source."
    }
    $Version = $Matches['version']
}

$zipFile = $zips | Where-Object { $_.Name -eq "ChyguiSlide_${Version}_x64-setup.zip" } |
    Select-Object -First 1
if (-not $zipFile) {
    throw "В папке release\ нет архива ChyguiSlide_${Version}_x64-setup.zip — выполните prepare_release.ps1."
}

# Части загружаются вместо архива: сам архив больше лимита GitVerse в 100 МБ.
$uploadFiles = @($partFiles | Where-Object { $_.Name -like "ChyguiSlide_${Version}_x64-setup.part*.gz" })
if ($uploadFiles.Count -eq 0) { $uploadFiles = @($zipFile) }

$sha256 = (Get-FileHash -Path $zipFile.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
$sizeBytes = $zipFile.Length
$tag = "v$Version"

Write-Host "[1/6] Версия $Version, файлов релиза: $($uploadFiles.Count)"
Write-Host (("      архив: {0} — {1:N1} МБ, sha256 {2}") -f $zipFile.Name, ($sizeBytes / 1MB), $sha256)
foreach ($file in $uploadFiles) {
    Write-Host ("      загружаем: {0} — {1:N1} МБ" -f $file.Name, ($file.Length / 1MB))
}

# --- 2. Релиз ----------------------------------------------------------------

if ($DryRun) {
    Write-Host ''
    Write-Host 'Пробный запуск (-DryRun): к сети не обращаемся, публикация пропущена.'
    Write-Host 'Файлы для загрузки в релиз:'
    foreach ($file in $uploadFiles) { Write-Host ("  release\{0}" -f $file.Name) }
    Write-Host 'Ссылки на файлы выдаёт GitVerse при загрузке ассета, поэтому блок'
    Write-Host 'манифеста собирается уже во время публикации и попадает в Update.md,'
    Write-Host 'а README.md публикуется как описание программы.'
    exit 0
}

if (-not $Token) {
    throw 'Не задан токен GitVerse. Создайте его в профиле (Управление токенами) и передайте' +
        ' параметром -Token или переменной окружения GITVERSE_TOKEN.'
}

$releaseBody = @"
ChyguiSlide $Version — установщик для Windows 10/11 (x64).

$Notes

Архив обновления: $sizeBytes байт, SHA-256 $sha256.

Файлы релиза — gzip-части архива с установщиком: приложение скачает их по
порядку, распакует, склеит и проверит SHA-256.

Установка на новый компьютер — одной строкой в PowerShell: скрипт скачает
части, сверит SHA-256 и запустит установщик.

  & ([scriptblock]::Create((irm '$contentBase/install.ps1?raw=1').TrimStart([char]0xFEFF)))
"@

$release = $null
$releasePayload = [ordered]@{
    tag_name           = $tag
    name               = "ChyguiSlide $Version"
    target_commitish   = $Branch
    body               = $releaseBody
    draft              = $false
    prerelease         = $false
    is_authorized_only = $false
} | ConvertTo-Json -Depth 5

try {
    $release = Invoke-GitVerseApi -Method POST -Path "/repos/$Owner/$Repo/releases" -Body $releasePayload
    Write-Host "[2/6] Релиз $tag создан (id $($release.id))"
} catch {
    Write-Host "      $($_.Exception.Message)"
    Write-Host "      ищу уже существующий релиз с тегом $tag"
    $releaseList = @(Invoke-GitVerseApi -Method GET -Path "/repos/$Owner/$Repo/releases")
    $release = $releaseList | Where-Object { $_.tag_name -eq $tag } | Select-Object -First 1
    if (-not $release) { throw }
    Write-Host "[2/6] Используется существующий релиз $tag (id $($release.id))"
    # Описание уже созданного релиза тоже приводим к текущему тексту: на
    # странице релиза пользователь ищет инструкцию по установке. Если GitVerse
    # правку не принимает — не беда, релиз и файлы уже на месте.
    try {
        $null = Invoke-GitVerseApi -Method PATCH -Path "/repos/$Owner/$Repo/releases/$($release.id)" `
            -Body (@{ body = $releaseBody } | ConvertTo-Json -Depth 5)
        Write-Host '      описание релиза обновлено'
    } catch {
        Write-Host "      описание релиза не обновилось: $($_.Exception.Message)"
    }
}

# --- 3. Загрузка файлов ------------------------------------------------------
# Адрес для манифеста выдаёт GitVerse при загрузке ассета — вид
# https://gitverse.ru/api/attachments/<идентификатор>. Собранные адреса
# сохраняются в том же порядке, в котором части склеиваются.

Write-Host '[3/6] Загрузка файлов релиза'
$releaseDetail = Invoke-GitVerseApi -Method GET -Path "/repos/$Owner/$Repo/releases/$($release.id)"
$knownAssets = @()
if ($releaseDetail.assets) { $knownAssets = @($releaseDetail.assets) }

$assetUrls = [ordered]@{}
foreach ($file in $uploadFiles) {
    $already = $knownAssets | Where-Object { $_.name -eq $file.Name } | Select-Object -First 1
    if ($already) {
        Write-Host ("      {0} — уже в релизе" -f $file.Name)
        $assetUrls[$file.Name] = $already.browser_download_url
        continue
    }
    $asset = Send-ReleaseAsset -ReleaseId $release.id -File $file
    Write-Host ("      {0} — {1:N1} МБ" -f $file.Name, ($file.Length / 1MB))
    $assetUrls[$file.Name] = $asset.browser_download_url
}

# --- 4. Манифест и Update.md -------------------------------------------------

$updateText = [System.IO.File]::ReadAllText($updatePath)
$blockMatch = [regex]::Match($updateText, [regex]::Escape($marker) + '\s*```json\s*(?<json>.*?)\s*```',
    [System.Text.RegularExpressions.RegexOptions]::Singleline)
if (-not $blockMatch.Success) {
    throw "В Update.md нет блока манифеста с маркером $marker."
}

# Что нового и обязательность берём из текущего Update.md, если не заданы параметром.
$previous = $null
try {
    $previous = $blockMatch.Groups['json'].Value | ConvertFrom-Json
} catch {
    Write-Host '      текущий блок манифеста не читается — беру значения по умолчанию'
}
if (-not $Notes) {
    if ($previous -and $previous.notes) {
        $Notes = $previous.notes
    } else {
        $Notes = "Что нового:`n- ..."
    }
}

$manifest = [ordered]@{
    version     = $Version
    publishedAt = (Get-Date -Format 'dd.MM.yyyy')
    mandatory   = [bool]($previous -and $previous.mandatory)
    notes       = $Notes
}
$urls = @($uploadFiles | ForEach-Object { $assetUrls[$_.Name] })
if ($urls.Count -eq 1) {
    $manifest['downloadUrl'] = $urls[0]
} else {
    $manifest['parts'] = $urls
}
$manifest['sha256'] = $sha256
$manifest['sizeBytes'] = $sizeBytes
$manifestJson = ConvertTo-ManifestJson -Manifest $manifest
# Проверка: блок должен читаться как JSON — его разбирает приложение.
$null = $manifestJson | ConvertFrom-Json

$block = @($marker, '```json', $manifestJson, '```') -join "`r`n"
$updated = $updateText.Substring(0, $blockMatch.Index) + $block + $updateText.Substring($blockMatch.Index + $blockMatch.Length)
[System.IO.File]::WriteAllText($updatePath, $updated, $utf8NoBom)

Write-Host '[4/6] Блок манифеста в Update.md обновлён:'
Write-Host ''
Write-Host $manifestJson
Write-Host ''

# --- 5. Update.md и README.md в репозитории ----------------------------------

if ($SkipDocs) {
    Write-Host '[5/6] Публикация файлов репозитория пропущена (-SkipDocs)'
} else {
    Write-Host '[5/6] Публикация файлов репозитория'
    # Update.md идёт первым: как только он опубликован, программа видит новую
    # версию, а README.md обновляется независимо от неё.
    $null = Set-RepoFile -Path 'Update.md' `
        -Content ([System.IO.File]::ReadAllText($updatePath)) `
        -Message "Update.md: описание обновления $Version"
    $null = Set-RepoFile -Path 'README.md' `
        -Content ([System.IO.File]::ReadAllText($readmePath)) `
        -Message 'README.md: описание программы'
    # BOM в install.ps1 обязателен: без него PowerShell 5.1 читает файл как ANSI
    # и кириллица в сообщениях скрипта превращается в мусор. ReadAllText BOM
    # отбрасывает, поэтому добавляем его обратно.
    $installContent = [string][char]0xFEFF + [System.IO.File]::ReadAllText($installPath)
    $null = Set-RepoFile -Path 'install.ps1' `
        -Content $installContent `
        -Message 'install.ps1: установка на новый компьютер'
}

# --- 6. Проверка -------------------------------------------------------------

Write-Host '[6/6] Проверка анонимного доступа'
if ($SkipDocs) {
    Write-Host '      проверка файлов репозитория пропущена (-SkipDocs)'
} else {
    $updateUrl = "$contentBase/Update.md?raw=1"
    $updateResponse = Invoke-WebRequest -UseBasicParsing -Uri $updateUrl -TimeoutSec 60
    if (-not $updateResponse.Content.Contains($marker)) {
        throw "Update.md по адресу $updateUrl скачивается, но без маркера $marker."
    }
    if (-not $updateResponse.Content.Contains("`"version`": `"$Version`"")) {
        throw "В опубликованном Update.md нет версии $Version — обновление не увидит релиз."
    }
    if (-not $updateResponse.Content.Contains($urls[0])) {
        throw "В опубликованном Update.md нет ссылок на файлы релиза."
    }
    Write-Host "      Update.md: $updateUrl — версия $Version и ссылки на месте"

    $readmeUrl = "$contentBase/README.md?raw=1"
    $readmeResponse = Invoke-WebRequest -UseBasicParsing -Uri $readmeUrl -TimeoutSec 60
    if ($readmeResponse.Content.Contains($marker)) {
        throw "В опубликованном README.md остался блок манифеста — он должен лежать только в Update.md."
    }
    Write-Host "      README.md: $readmeUrl — описание программы без блока манифеста"

    $installUrl = "$contentBase/install.ps1?raw=1"
    $installResponse = Invoke-WebRequest -UseBasicParsing -Uri $installUrl -TimeoutSec 60
    if ($installResponse.Content -match '<!DOCTYPE|<html') {
        throw "install.ps1 по адресу $installUrl отдаётся как страница, а не как файл."
    }
    if (-not $installResponse.Content.Contains('CHYGUISLIDE-UPDATE')) {
        throw "В опубликованном install.ps1 нет разбора блока манифеста — файл скачался не тот."
    }
    Write-Host "      install.ps1: $installUrl — скрипт установки на месте"
}

foreach ($file in $uploadFiles) {
    $url = $assetUrls[$file.Name]
    # Запрашиваем один байт: доступность проверяется без скачивания всего файла.
    $fileResponse = $null
    try {
        $request = [System.Net.HttpWebRequest]::Create($url)
        $request.AddRange(0, 0)
        $request.Timeout = 60000
        $fileResponse = $request.GetResponse()
        $code = [int]$fileResponse.StatusCode
    } catch {
        $code = 0
        if ($_.Exception.Response) { $code = [int]$_.Exception.Response.StatusCode }
    } finally {
        if ($fileResponse) { $fileResponse.Close() }
    }
    if ($code -ne 200 -and $code -ne 206) {
        throw "Файл $($file.Name) не скачивается анонимно: $url — код $code." +
            ' Проверьте в релизе флажок «Ограничение доступа для неавторизованных пользователей».'
    }
    Write-Host "      $($file.Name): код $code"
}

if ($VerifyDownload) {
    Write-Host '      склейка частей и сверка SHA-256 (скачиваем файлы целиком)'
    $temp = Join-Path $env:TEMP ("chyguislide-verify-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $temp | Out-Null
    try {
        $assembled = Join-Path $temp 'update.zip'
        $out = [System.IO.File]::Create($assembled)
        try {
            foreach ($file in $uploadFiles) {
                $partPath = Join-Path $temp $file.Name
                Invoke-WebRequest -UseBasicParsing -Uri $assetUrls[$file.Name] -OutFile $partPath -TimeoutSec 1800
                $in = [System.IO.File]::OpenRead($partPath)
                try {
                    if ($file.Name -like '*.gz') {
                        $gzip = [System.IO.Compression.GzipStream]::new(
                            $in, [System.IO.Compression.CompressionMode]::Decompress)
                        try { $gzip.CopyTo($out) } finally { $gzip.Dispose() }
                    } else {
                        $in.CopyTo($out)
                    }
                } finally { $in.Dispose() }
                Remove-Item $partPath -Force
            }
        } finally { $out.Dispose() }
        $check = (Get-FileHash -Path $assembled -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($check -ne $sha256) {
            throw "SHA-256 склеенного файла $check не совпал с манифестом $sha256."
        }
        Write-Host "      SHA-256 совпал: $check"
    } finally {
        Remove-Item $temp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Host ''
Write-Host "Готово. Релиз: $repoUrl/releases/tag/$tag"
Write-Host "Файлы репозитория: $repoUrl (README.md, Update.md)"


