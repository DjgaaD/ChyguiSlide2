<#
    publish_github.ps1 — публикует релиз ChyguiSlide и описание обновления на GitHub.

    Что делает:
      1. берёт версию из src-tauri/tauri.conf.json и файлы из папки release/
         (их готовит prepare_release.ps1);
      2. создаёт релиз с тегом v<версия> или дополняет уже созданный;
      3. загружает файлы релиза через gh — GitHub принимает .exe напрямую;
      4. собирает блок манифеста и подставляет его в Update.md;
      5. публикует Update.md, README.md и install.ps1 в репозиторий;
      6. проверяет, что файл релиза и документация скачиваются анонимно.

    Нужен GitHub CLI (https://cli.github.com), авторизованный в нужном аккаунте:
    gh auth status. Токен нигде в проекте не хранится — gh держит его у себя.

    Описание обновления живёт в отдельном файле Update.md, а README.md остаётся
    описанием программы. Текст изменений (`notes`) сохраняется: скрипт обновляет
    в блоке только версию, дату, ссылки и контрольную сумму.

    Запуск:
      powershell -ExecutionPolicy Bypass -File publish_github.ps1 -DryRun
      powershell -ExecutionPolicy Bypass -File publish_github.ps1
      powershell -ExecutionPolicy Bypass -File publish_github.ps1 -VerifyDownload

    Ключи:
      -Version <версия>  — версия релиза (по умолчанию из tauri.conf.json);
      -Notes <текст>     — список изменений для манифеста и страницы релиза;
      -NotesFile <файл>  — то же, но текстом из файла;
      -Owner, -Repo      — куда публиковать (по умолчанию DjgaaD/ChyguiSlide2);
      -Branch            — ветка с документацией (по умолчанию master);
      -DryRun            — показать план без обращений к сети;
      -SkipRelease       — не трогать релиз, обновить только документацию;
      -SkipDocs          — не публиковать README.md, Update.md и install.ps1;
      -VerifyDownload    — скачать файл релиза целиком и сверить SHA-256.

    Код возврата: 0 — успех, 1 — ошибка.
#>
[CmdletBinding()]
param(
    [string]$Owner = 'DjgaaD',
    [string]$Repo = 'ChyguiSlide2',
    [string]$Branch = 'master',
    [string]$Version,
    [string]$Notes,
    [string]$NotesFile,
    [switch]$DryRun,
    [switch]$SkipDocs,
    [switch]$SkipRelease,
    [switch]$VerifyDownload
)

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repoFull = "$Owner/$Repo"
$webUrl = "https://github.com/$repoFull"
$rawBase = "https://raw.githubusercontent.com/$repoFull/$Branch"
$marker = '<!-- CHYGUISLIDE-UPDATE -->'
$utf8Bom = New-Object System.Text.UTF8Encoding $true

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$releaseDir = Join-Path $root 'release'
$readmePath = Join-Path $root 'README.md'
$updatePath = Join-Path $root 'Update.md'
$installPath = Join-Path $root 'install.ps1'
$configPath = Join-Path $root 'src-tauri\tauri.conf.json'

# --- Вспомогательные функции -------------------------------------------------

# Запуск gh: возвращает код возврата и строки вывода.
function Invoke-Gh {
    param(
        [Parameter(Mandatory)][string[]]$Arguments,
        [switch]$AllowFailure
    )
    $output = & gh @Arguments 2>&1
    $code = $LASTEXITCODE
    if ($code -ne 0 -and -not $AllowFailure) {
        throw "gh $($Arguments -join ' ') завершился с кодом ${code}:`n$($output -join "`n")"
    }
    return [pscustomobject]@{ Code = $code; Output = @($output) }
}

# Строка JSON в кавычках: так экранируются кавычки и переводы строк.
function ConvertTo-JsonString {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Value)
    return (ConvertTo-Json -InputObject $Value -Compress)
}

# Строки файла и границы блока манифеста в нём: строка-маркер, открывающая и
# закрывающая ограда ``` и разобранный JSON между ними.
function Get-ManifestBlock {
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $text = [System.IO.File]::ReadAllText($Path)
    $lines = $text -split "`r?`n"

    $markerIndex = -1
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i].Trim() -eq $marker) { $markerIndex = $i; break }
    }
    if ($markerIndex -lt 0) { return $null }

    $openIndex = -1
    for ($i = $markerIndex + 1; $i -lt $lines.Count; $i++) {
        if ($lines[$i].TrimStart().StartsWith('```')) { $openIndex = $i; break }
    }
    if ($openIndex -lt 0) { return $null }

    $closeIndex = -1
    for ($i = $openIndex + 1; $i -lt $lines.Count; $i++) {
        if ($lines[$i].TrimStart().StartsWith('```')) { $closeIndex = $i; break }
    }
    if ($closeIndex -lt 0) { return $null }

    $json = $null
    try {
        $json = (($lines[($openIndex + 1)..($closeIndex - 1)] -join "`n") | ConvertFrom-Json)
    } catch {
        $json = $null
    }

    $newLine = "`n"
    if ($text.Contains("`r`n")) { $newLine = "`r`n" }
    return [pscustomobject]@{
        MarkerIndex = $markerIndex
        OpenIndex   = $openIndex
        CloseIndex  = $closeIndex
        Json        = $json
        NewLine     = $newLine
    }
}

# Значение поля блока, если поле есть (для сохранения ручных правок).
function Get-BlockField {
    param($Block, [Parameter(Mandatory)][string]$Name)
    if (-not $Block -or -not $Block.Json) { return $null }
    if ($Block.Json.PSObject.Properties.Name -notcontains $Name) { return $null }
    return $Block.Json.$Name
}

# Публикация файлов в репозиторий: обычным push, если проект — git-репозиторий,
# иначе через API GitHub (тогда коммит создаёт сам GitHub).
function Publish-Docs {
    param(
        [Parameter(Mandatory)][string[]]$Paths,
        [Parameter(Mandatory)][string]$Message
    )
    $relative = @($Paths | ForEach-Object { $_.Substring($root.Length + 1) })

    $inside = & git rev-parse --is-inside-work-tree 2>$null
    if ($LASTEXITCODE -eq 0 -and "$inside".Trim() -eq 'true') {
        & git add -- $relative
        if ($LASTEXITCODE -ne 0) { throw 'git add не удался.' }
        $pending = @(& git status --porcelain -- $relative)
        if ($pending.Count -gt 0) {
            & git commit -m $Message
            if ($LASTEXITCODE -ne 0) { throw 'git commit не удался.' }
        } else {
            Write-Host '      изменения уже закоммичены'
        }
        & git push
        if ($LASTEXITCODE -ne 0) {
            throw 'git push не удался — проверьте доступ к репозиторию (gh auth setup-git).'
        }
        Write-Host '      документация отправлена в репозиторий (git push)'
        return
    }

    foreach ($path in $Paths) {
        $name = $path.Substring($root.Length + 1).Replace('\', '/')
        $sha = $null
        $probe = Invoke-Gh -Arguments @('api', "repos/$repoFull/contents/${name}?ref=$Branch") -AllowFailure
        if ($probe.Code -eq 0) {
            $sha = ((($probe.Output) -join "`n") | ConvertFrom-Json).sha
        }

        $base64 = [Convert]::ToBase64String([System.IO.File]::ReadAllBytes($path))
        $arguments = @('api', '--method', 'PUT', "repos/$repoFull/contents/$name",
            '-f', "message=$Message", '-f', "content=$base64", '-f', "branch=$Branch")
        if ($sha) { $arguments += @('-f', "sha=$sha") }

        $result = Invoke-Gh -Arguments $arguments -AllowFailure
        if ($result.Code -ne 0 -and -not $sha) {
            # Ветки может ещё не быть (пустой репозиторий) — GitHub создаст её
            # первым коммитом, если ветку не указывать.
            $withoutBranch = @($arguments | Where-Object { $_ -ne "branch=$Branch" })
            $result = Invoke-Gh -Arguments $withoutBranch
        } elseif ($result.Code -ne 0) {
            throw "Не удалось опубликовать $name`n$($result.Output -join "`n")"
        }
        Write-Host "      $name опубликован (API GitHub)"
    }
}

# --- 1. Версия и файлы релиза ------------------------------------------------

Write-Host '[1/6] Версия и файлы релиза'

if (-not $Version) {
    if (-not (Test-Path -LiteralPath $configPath)) {
        throw "Не найден $configPath — укажите версию ключом -Version."
    }
    $Version = [string](((Get-Content -LiteralPath $configPath -Raw) | ConvertFrom-Json).version)
}
if (-not $Version) { throw 'Не удалось определить версию релиза.' }
$tag = "v$Version"

if (-not (Test-Path -LiteralPath $releaseDir)) {
    throw 'Не найдена папка release — сначала выполните prepare_release.ps1.'
}
$releaseFiles = @(Get-ChildItem -LiteralPath $releaseDir -File | Sort-Object Name)
if ($releaseFiles.Count -eq 0) {
    throw 'Папка release пуста — сначала выполните prepare_release.ps1.'
}

# gzip-части означают, что крупный архив разбит на куски. Если частей нет, в
# манифест идёт одна ссылка на файл обновления.
$partFiles = @($releaseFiles | Where-Object { $_.Name -like '*.gz' })
$singleFiles = @($releaseFiles | Where-Object { $_.Extension -in '.exe', '.zip' })
if ($partFiles.Count -eq 0 -and $singleFiles.Count -eq 0) {
    throw "В папке release нет ни .exe, ни .zip, ни gzip-частей: $($releaseFiles.Name -join ', ')."
}

$singleFile = $null
if ($singleFiles.Count -gt 0) {
    $singleFile = $singleFiles | Sort-Object LastWriteTime -Descending | Select-Object -First 1
}
if ($singleFile -and $singleFile.Name -notlike "*$Version*") {
    Write-Host "      внимание: имя $($singleFile.Name) не содержит версию $Version"
}

# Контрольная сумма считается от того файла, который скачает программа: от
# установщика (одна ссылка) или от целого архива, части которого лежат в релизе.
if ($partFiles.Count -gt 0) {
    $hashFile = $singleFiles | Where-Object { $_.Extension -eq '.zip' } | Select-Object -First 1
    if (-not $hashFile) {
        throw ('В релизе есть gzip-части, но нет самого архива .zip: без него нельзя ' +
            'посчитать sha256 для манифеста. Положите архив в release/ или уберите части.')
    }
} else {
    $hashFile = $singleFile
}
$sizeBytes = $hashFile.Length
$sha256 = (Get-FileHash -LiteralPath $hashFile.FullName -Algorithm SHA256).Hash.ToLowerInvariant()

foreach ($file in $releaseFiles) {
    Write-Host ('      {0} — {1:N1} МБ' -f $file.Name, ($file.Length / 1MB))
}
Write-Host "      версия: $Version, sha256: $sha256"

# --- 2. Текст изменений и пробный запуск -------------------------------------

$block = Get-ManifestBlock -Path $updatePath
$notesText = $null
if ($NotesFile) {
    if (-not (Test-Path -LiteralPath $NotesFile)) { throw "Не найден файл с описанием: $NotesFile" }
    $notesText = [System.IO.File]::ReadAllText((Resolve-Path -LiteralPath $NotesFile).Path).Trim()
} elseif ($Notes) {
    $notesText = $Notes.Trim()
}
# Без -Notes текст изменений берётся из текущего блока Update.md: правки автора
# сохраняются, скрипт обновляет только версию, дату, ссылки и контрольную сумму.
if (-not $notesText) { $notesText = [string](Get-BlockField -Block $block -Name 'notes') }
if (-not $notesText) { $notesText = "Версия $Version." }

$mandatory = [bool](Get-BlockField -Block $block -Name 'mandatory')
$minSupported = [string](Get-BlockField -Block $block -Name 'minSupported')

if ($DryRun) {
    Write-Host ''
    Write-Host '[2/6] Пробный запуск — обращений к сети не будет'
    Write-Host "      релиз: $webUrl/releases/tag/$tag"
    Write-Host "      файлов в релизе: $($releaseFiles.Count) ($($releaseFiles.Name -join ', '))"
    Write-Host "      манифест: $(if ($partFiles.Count -gt 0) { "части, $($partFiles.Count) ссылок" } else { 'одна ссылка downloadUrl' })"
    Write-Host "      sha256: $sha256"
    Write-Host "      текст изменений: $($notesText -split "`n" | Select-Object -First 1)"
    Write-Host "      обязательное обновление: $mandatory"
    Write-Host ''
    Write-Host 'Пробный запуск завершён.'
    exit 0
}

# --- 3. gh CLI ---------------------------------------------------------------

Write-Host '[2/6] Проверка gh'
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    throw 'Не найден gh (GitHub CLI). Установите его (winget install GitHub.cli) и выполните gh auth login.'
}
$auth = Invoke-Gh -Arguments @('auth', 'status') -AllowFailure
if ($auth.Code -ne 0) {
    throw "gh не авторизован — выполните gh auth login.`n$($auth.Output -join "`n")"
}
Write-Host '      gh на месте и авторизован'

# --- 4. Релиз ----------------------------------------------------------------

$assetUrls = @{}
if ($SkipRelease) {
    Write-Host '[3/6] Релиз не трогаем (-SkipRelease)'
    foreach ($file in $releaseFiles) {
        $assetUrls[$file.Name] = "$webUrl/releases/download/$tag/$($file.Name)"
    }
} else {
    Write-Host "[3/6] Релиз $tag"
    $view = Invoke-Gh -Arguments @('release', 'view', $tag, '-R', $repoFull, '--json', 'tagName') -AllowFailure
    if ($view.Code -eq 0) {
        Write-Host "      релиз $tag уже есть — дополняем его"
    } else {
        $installLine = "& ([scriptblock]::Create((irm '$rawBase/install.ps1').TrimStart([char]0xFEFF)))"
        $body = @(
            $notesText,
            '',
            '**Установка на новый компьютер.** Откройте PowerShell и выполните строку:',
            '',
            '```powershell',
            $installLine,
            '```',
            '',
            'Установщик не подписан сертификатом, поэтому Windows может показать',
            'предупреждение SmartScreen: «Подробнее» → «Выполнить в любом случае».',
            '',
            "SHA-256 файла обновления: ``$sha256``",
            '',
            'Машинночитаемое описание версии — в [Update.md](Update.md).'
        ) -join "`r`n"
        $bodyFile = Join-Path $env:TEMP 'chyguislide-release-notes.txt'
        [System.IO.File]::WriteAllText($bodyFile, $body, $utf8Bom)
        Invoke-Gh -Arguments @('release', 'create', $tag, '-R', $repoFull,
            '--title', "ChyguiSlide $Version", '--notes-file', $bodyFile) | Out-Null
        Remove-Item $bodyFile -Force -ErrorAction SilentlyContinue
        Write-Host "      релиз $tag создан"
    }

    Write-Host "      загрузка файлов: $($releaseFiles.Name -join ', ')"
    $uploadPaths = @($releaseFiles | ForEach-Object { $_.FullName })
    Invoke-Gh -Arguments (@('release', 'upload', $tag, '-R', $repoFull, '--clobber') + $uploadPaths) | Out-Null

    # Ссылки берём из ответа GitHub — это ровно те адреса, которые скачает программа.
    $releaseJson = ((Invoke-Gh -Arguments @('api', "repos/$repoFull/releases/tags/$tag")).Output) -join "`n"
    foreach ($asset in ($releaseJson | ConvertFrom-Json).assets) {
        $assetUrls[$asset.name] = $asset.browser_download_url
    }
    foreach ($file in $releaseFiles) {
        if (-not $assetUrls.ContainsKey($file.Name)) {
            throw "Файл $($file.Name) не появился в релизе $tag."
        }
        Write-Host "      $($file.Name): $($assetUrls[$file.Name])"
    }
}

# --- 5. Блок манифеста в Update.md -------------------------------------------

$partUrls = @($partFiles | ForEach-Object { $assetUrls[$_.Name] })
$jsonLines = New-Object System.Collections.Generic.List[string]
$jsonLines.Add('{')
$jsonLines.Add('  "version": ' + (ConvertTo-JsonString $Version) + ',')
$jsonLines.Add('  "publishedAt": ' + (ConvertTo-JsonString (Get-Date -Format 'dd.MM.yyyy')) + ',')
$jsonLines.Add('  "mandatory": ' + $(if ($mandatory) { 'true' } else { 'false' }) + ',')
if ($minSupported) {
    $jsonLines.Add('  "minSupported": ' + (ConvertTo-JsonString $minSupported) + ',')
}
$jsonLines.Add('  "notes": ' + (ConvertTo-JsonString $notesText) + ',')
if ($partUrls.Count -gt 0) {
    $jsonLines.Add('  "parts": [')
    for ($i = 0; $i -lt $partUrls.Count; $i++) {
        $tail = ','
        if ($i -eq $partUrls.Count - 1) { $tail = '' }
        $jsonLines.Add('    ' + (ConvertTo-JsonString $partUrls[$i]) + $tail)
    }
    $jsonLines.Add('  ],')
} else {
    $jsonLines.Add('  "downloadUrl": ' + (ConvertTo-JsonString $assetUrls[$singleFile.Name]) + ',')
}
$jsonLines.Add('  "sha256": ' + (ConvertTo-JsonString $sha256) + ',')
$jsonLines.Add('  "sizeBytes": ' + $sizeBytes)
$jsonLines.Add('}')

if ($SkipDocs) {
    Write-Host '[4/6] Документация не публикуется (-SkipDocs)'
} else {
    Write-Host '[4/6] Публикация README.md, Update.md и install.ps1'
    if (-not (Test-Path -LiteralPath $updatePath)) {
        throw "Не найден $updatePath — без него программа не увидит обновление."
    }
    if (-not $block) {
        throw "В $updatePath нет блока манифеста: нужна строка $marker и следом ограда с JSON."
    }
    $text = [System.IO.File]::ReadAllText($updatePath)
    $lines = $text -split "`r?`n"
    $head = @($lines[0..$block.MarkerIndex])
    $tail = @()
    if ($block.CloseIndex + 1 -lt $lines.Count) {
        $tail = @($lines[($block.CloseIndex + 1)..($lines.Count - 1)])
    }
    $newLines = @($head) + @('```json') + @($jsonLines) + @('```') + @($tail)
    $newText = ($newLines -join $block.NewLine)
    if (-not $newText.Contains($marker) -or -not $newText.Contains($Version)) {
        throw 'Собранный Update.md не похож на манифест — публикация отменена.'
    }
    [System.IO.File]::WriteAllText($updatePath, $newText, $utf8Bom)
    $linkCount = 1
    if ($partUrls.Count -gt 0) { $linkCount = $partUrls.Count }
    Write-Host "      Update.md обновлён: версия $Version, ссылок в манифесте: $linkCount"

    $docs = @($updatePath, $readmePath, $installPath) | Where-Object { Test-Path -LiteralPath $_ }
    Publish-Docs -Paths $docs -Message "Описание версии $Version"
}

# --- 6. Проверка -------------------------------------------------------------

Write-Host '[5/6] Проверка скачивания'

if (-not $SkipRelease) {
    $probeUrl = $assetUrls[$hashFile.Name]
    $code = 0
    $response = $null
    try {
        $request = [System.Net.HttpWebRequest]::Create($probeUrl)
        # Запрашиваем один байт: доступность проверяется без скачивания всего файла.
        $request.AddRange(0, 0)
        $request.Timeout = 60000
        $response = $request.GetResponse()
        $code = [int]$response.StatusCode
    } catch {
        if ($_.Exception.Response) { $code = [int]$_.Exception.Response.StatusCode }
    } finally {
        if ($response) { $response.Close() }
    }
    if ($code -ne 200 -and $code -ne 206) {
        throw "Файл релиза не скачивается анонимно: $probeUrl — код $code."
    }
    Write-Host "      $probeUrl — код $code"
}

if (-not $SkipDocs) {
    # Приписка в адресе обходит кэш CDN: сразу после публикации он ещё отдаёт
    # прежнюю копию файла.
    $cacheBuster = [DateTime]::UtcNow.Ticks
    $updateResponse = Invoke-WebRequest -UseBasicParsing -Uri "$rawBase/Update.md?cache=$cacheBuster" -TimeoutSec 60
    if (-not $updateResponse.Content.Contains($marker)) {
        throw "В опубликованном Update.md нет блока манифеста — проверьте $rawBase/Update.md."
    }
    if ($updateResponse.Content -notmatch [regex]::Escape("`"version`": `"$Version`"")) {
        throw "В опубликованном Update.md нет версии $Version — возможно, отдаётся старая копия."
    }
    Write-Host "      Update.md: $rawBase/Update.md — версия $Version на месте"

    $readmeResponse = Invoke-WebRequest -UseBasicParsing -Uri "$rawBase/README.md?cache=$cacheBuster" -TimeoutSec 60
    if ($readmeResponse.Content.Contains($marker)) {
        throw 'В опубликованном README.md остался блок манифеста — он должен лежать только в Update.md.'
    }
    Write-Host '      README.md: описание программы без блока манифеста'

    $installUrl = "$rawBase/install.ps1?cache=$cacheBuster"
    $installResponse = Invoke-WebRequest -UseBasicParsing -Uri $installUrl -TimeoutSec 60
    if ($installResponse.Content -match '<!DOCTYPE|<html') {
        throw "install.ps1 по адресу $installUrl отдаётся как страница, а не как файл."
    }
    if (-not $installResponse.Content.Contains('CHYGUISLIDE-UPDATE')) {
        throw 'В опубликованном install.ps1 нет разбора блока манифеста — скачался не тот файл.'
    }
    Write-Host '      install.ps1: скрипт установки на месте'
}

if ($VerifyDownload) {
    Write-Host '[6/6] Скачивание файла релиза целиком и сверка SHA-256'
    Add-Type -AssemblyName System.IO.Compression | Out-Null
    $temp = Join-Path $env:TEMP ('chyguislide-verify-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $temp | Out-Null
    try {
        $assembled = Join-Path $temp 'update.bin'
        $out = [System.IO.File]::Create($assembled)
        try {
            foreach ($file in $releaseFiles) {
                $partPath = Join-Path $temp $file.Name
                Invoke-WebRequest -UseBasicParsing -Uri $assetUrls[$file.Name] -OutFile $partPath -TimeoutSec 3600
                $in = [System.IO.File]::OpenRead($partPath)
                try {
                    if ($file.Name -like '*.gz') {
                        $gzip = New-Object System.IO.Compression.GzipStream(
                            $in, [System.IO.Compression.CompressionMode]::Decompress)
                        try { $gzip.CopyTo($out) } finally { $gzip.Dispose() }
                    } else {
                        $in.CopyTo($out)
                    }
                } finally { $in.Dispose() }
                Remove-Item $partPath -Force
            }
        } finally { $out.Dispose() }
        $check = (Get-FileHash -LiteralPath $assembled -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($check -ne $sha256) {
            throw "SHA-256 скачанного файла $check не совпал с манифестом $sha256."
        }
        Write-Host "      SHA-256 совпал: $check"
    } finally {
        Remove-Item $temp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Host ''
Write-Host "Готово. Релиз: $webUrl/releases/tag/$tag"
Write-Host "Документация: $webUrl (README.md, Update.md, install.ps1)"
Write-Host 'Установка на новом компьютере — одна строка в PowerShell:'
Write-Host "  & ([scriptblock]::Create((irm '$rawBase/install.ps1').TrimStart([char]0xFEFF)))"
if ($PSCommandPath) { exit 0 }
