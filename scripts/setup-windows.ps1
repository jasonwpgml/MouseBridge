<#
Installs this fork's Lan Mouse on Windows and points it at a Mac.
setup-mac.sh prints the full command; by hand:

  setup-windows.ps1 -MacName Jason-MacAir -MacIp 192.168.0.10 -Side bottom

-Side is where the Mac's screen sits next to this PC (left|right|top|bottom).
-MacFingerprint pre-trusts the Mac so no Authorize dialog shows up here.
Keys sent to the Mac swap Ctrl and Win and turn 한/영 (or right Alt) into F18.
#>
param(
    [string]$MacName = 'mac',
    [string]$MacIp,
    [ValidateSet('left', 'right', 'top', 'bottom')][string]$Side,
    [string]$MacFingerprint,
    [string]$Repo = 'jasonwpgml/lan-mouse',
    [string]$Tag
)
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5 downloads many times slower with the progress bar on
$ProgressPreference = 'SilentlyContinue'

function Step($text) { Write-Host "`n== $text" -ForegroundColor Cyan }

if (-not $Side) {
    do { $Side = Read-Host 'Mac 화면이 이 PC 화면의 어느 쪽에 있나요? (left/right/top/bottom)' }
    until ($Side -in 'left', 'right', 'top', 'bottom')
}
if (-not $MacIp) {
    # some ISP resolvers answer .local names with a public address
    $MacIp = Resolve-DnsName "$MacName.local" -Type A -ErrorAction SilentlyContinue |
        Where-Object { $_.IPAddress -match '^(10\.|192\.168\.|172\.(1[6-9]|2[0-9]|3[01])\.)' } |
        Select-Object -First 1 -ExpandProperty IPAddress
    if (-not $MacIp) { $MacIp = Read-Host "Mac($MacName)의 IP를 입력하세요" }
}

Step 'Lan Mouse 내려받기'
$api = "https://api.github.com/repos/$Repo/releases?per_page=1"
if ($Tag) { $api = "https://api.github.com/repos/$Repo/releases/tags/$Tag" }
$release = @(Invoke-RestMethod $api)[0]
$asset = $release.assets | Where-Object name -eq 'lan-mouse-windows-x86_64.zip'
if (-not $asset) { throw "$Repo 릴리스에서 lan-mouse-windows-x86_64.zip 을 찾지 못했어요." }
Write-Host $asset.browser_download_url

$zip = Join-Path $env:TEMP 'lan-mouse-windows-x86_64.zip'
Invoke-WebRequest $asset.browser_download_url -OutFile $zip -UseBasicParsing

Get-Process lan-mouse -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1
$dir = Join-Path $env:LOCALAPPDATA 'Programs\lan-mouse'
if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
Expand-Archive $zip $dir
Remove-Item $zip
$exe = Join-Path $dir 'lan-mouse.exe'
Write-Host "설치: $dir"

Step '설정'
$confDir = Join-Path $env:LOCALAPPDATA 'lan-mouse'
$conf = Join-Path $confDir 'config.toml'
New-Item -ItemType Directory -Force $confDir | Out-Null

# keep devices already trusted through the Authorize dialog
$trusted = [System.Collections.Generic.List[string]]::new()
if (Test-Path $conf) {
    Copy-Item $conf "$conf.bak-$(Get-Date -Format yyyyMMdd-HHmmss)"
    $keep = $false
    foreach ($line in Get-Content $conf) {
        if ($line -match '^\[') {
            $keep = $line -eq '[authorized_fingerprints]'
            continue
        }
        if ($keep -and $line.Trim()) { $trusted.Add($line) }
    }
}
if ($MacFingerprint -and -not ($trusted | Where-Object { $_ -like "*$MacFingerprint*" })) {
    $trusted.Add("`"$MacFingerprint`" = `"$MacName`"")
}

$lines = @(
    '# setup-windows.ps1 가 만든 파일 - 배치를 바꾸려면 스크립트를 다시 실행하세요'
    ''
    '[[clients]]'
    "hostname = `"$MacName`""
    "position = `"$Side`""
    "ips = [`"$MacIp`"]"
    'activate_on_startup = true'
    ''
    "[key_remap.`"$MacName`"]"
    'KeyLeftCtrl = "KeyLeftMeta"'
    'KeyLeftMeta = "KeyLeftCtrl"'
    'KeyRightCtrl = "KeyRightmeta"'
    'KeyRightmeta = "KeyRightCtrl"'
    'KeyHanguel = "KeyF18"'
    '# laptops often put 한/영 on the right Alt key'
    'KeyRightalt = "KeyF18"'
)
if ($trusted.Count) { $lines += @('', '[authorized_fingerprints]') + $trusted }
# the TOML parser rejects a byte order mark, which Set-Content would add on PowerShell 5
[System.IO.File]::WriteAllLines($conf, [string[]]$lines, [System.Text.UTF8Encoding]::new($false))
Write-Host "설정: $conf"

$shell = New-Object -ComObject WScript.Shell
# Start menu: the window, for settings and authorizing devices
$shortcut = $shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Programs')) 'Lan Mouse.lnk'))
$shortcut.TargetPath = $exe
$shortcut.WorkingDirectory = $dir
$shortcut.Save()

# Startup: the service on its own, without a console window. The window
# attaches to a running service and leaves it running when closed.
$conhost = Join-Path $env:SystemRoot 'System32\conhost.exe'
$daemonArgs = "--headless `"$exe`" daemon"
$autostart = $shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Startup')) 'Lan Mouse.lnk'))
$autostart.TargetPath = $conhost
$autostart.Arguments = $daemonArgs
$autostart.WorkingDirectory = $dir
$autostart.Save()

Start-Process $conhost -ArgumentList $daemonArgs -WorkingDirectory $dir

Step '남은 일'
Write-Host "1. 방화벽 창이 뜨면 '개인 네트워크' 허용"
Write-Host '2. Mac 설정이 끝나면 화면 가장자리로 커서를 넘겨 확인'
Write-Host '로그인하면 Lan Mouse가 창 없이 자동으로 켜져요. 설정은 시작 메뉴의 Lan Mouse로 열고, 창을 닫아도 계속 동작해요.'
