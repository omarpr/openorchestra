<#
.SYNOPSIS
  openOrchestra developer entry points on Windows (the Makefile's targets).

.DESCRIPTION
  Run through `make <target>` (the Makefile hands every target here on
  Windows; WinLibs ships make as mingw32-make) or `.\make <target>` (the
  make.cmd wrapper at the repo root, which needs no make at all).

    setup      check the build toolchain; exits non-zero if a required piece is missing
    doctor     report every build and runtime tool, required and optional
    build      build the debug app (and the phone web client when node >= 20 + pnpm exist)
    run        build, then run the debug app (extra args go to the app)
    check      cargo check the app
    test       headless workspace tests (harness-app excluded, as on macOS)
    web        build the oo-remote PWA (web/remote/dist, embedded by crates/remote)
    icon       regenerate assets/openOrchestra.ico (the exe/window icon) from the 1024px PNG
    dist       release build packaged as dist\openOrchestra-windows-x64(.zip)
    install    dist, then install per user (%LOCALAPPDATA%\Programs\openOrchestra) + Start menu shortcut
    uninstall  remove what install (and the app's notification registration) added

  The toolchain is per user and needs no admin: rustup with the GNU host
  (x86_64-pc-windows-gnu) and WinLibs GCC, MSVCRT build (Rust's GNU target
  links msvcrt). WinLibs is found on PATH, in $env:OO_MINGW, or in
  ~\tools\mingw64.
#>
param(
    [Parameter(Position = 0)] [string] $Target = "help",
    [Parameter(ValueFromRemainingArguments = $true)] [string[]] $Rest
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$App = Join-Path $Root "crates\harness-app"

function Write-Ok($m)   { Write-Host "  [ok] $m" -ForegroundColor Green }
function Write-Bad($m)  { Write-Host "  [missing] $m" -ForegroundColor Red }
function Write-Warn($m) { Write-Host "  [!] $m" -ForegroundColor Yellow }
function Write-Sec($m)  { Write-Host ""; Write-Host $m -ForegroundColor White }
function Test-Cmd($name) { [bool](Get-Command $name -ErrorAction SilentlyContinue) }

# Native commands don't throw on failure; this does.
function Invoke-Native {
    param([string] $Exe, [string[]] $Arguments)
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Exe $($Arguments -join ' ') failed (exit $LASTEXITCODE)" }
}

# The WinLibs bin directory, wherever it is.
function Find-MinGW {
    $candidates = @()
    if ($env:OO_MINGW) { $candidates += $env:OO_MINGW, (Join-Path $env:OO_MINGW "bin") }
    $gcc = Get-Command gcc -ErrorAction SilentlyContinue
    if ($gcc) { $candidates += (Split-Path $gcc.Source) }
    $candidates += (Join-Path $env:USERPROFILE "tools\mingw64\bin"), "C:\mingw64\bin"
    foreach ($c in $candidates) {
        if ($c -and (Test-Path (Join-Path $c "gcc.exe"))) { return (Resolve-Path $c).Path }
    }
    return $null
}

# PATH for cargo: rustup's bin and WinLibs first.
function Use-Toolchain {
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    $mingw = Find-MinGW
    if (-not $mingw) {
        throw "WinLibs GCC not found. Run: .\make setup"
    }
    $env:PATH = "$cargoBin;$mingw;$env:PATH"
    # Several git dependencies fetch more reliably through the git CLI.
    $env:CARGO_NET_GIT_FETCH_WITH_CLI = "true"
}

function Get-GitBash {
    foreach ($p in @("$env:ProgramFiles\Git\bin\bash.exe", "$env:LOCALAPPDATA\Programs\Git\bin\bash.exe")) {
        if (Test-Path $p) { return $p }
    }
    return $null
}

function Get-WebView2Version {
    foreach ($k in @(
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        "HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}")) {
        $v = (Get-ItemProperty $k -ErrorAction SilentlyContinue).pv
        if ($v -and $v -ne "0.0.0.0") { return $v }
    }
    return $null
}

function Get-NodeMajor {
    if (-not (Test-Cmd node)) { return 0 }
    $v = (& node --version) -replace '^v', ''
    return [int]($v.Split('.')[0])
}

# The pnpm the web client pins (`packageManager` in web\remote\package.json).
function Get-PinnedPnpm {
    $pkg = Get-Content (Join-Path $Root "web\remote\package.json") -Raw | ConvertFrom-Json
    if ($pkg.packageManager -match '^pnpm@') { return $pkg.packageManager -replace '\+.*$', '' }
    return "pnpm"
}

# How to run pnpm: itself, through corepack, or (Node 25+ ships no corepack)
# the pinned version through npx. npx.cmd, not npx: a .ps1 shim can be
# blocked by the execution policy.
function Get-PnpmCommand {
    if (Test-Cmd pnpm) { return @("pnpm") }
    if (Test-Cmd corepack) { return @("corepack", "pnpm") }
    if (Test-Cmd npx.cmd) { return @("npx.cmd", "-y", (Get-PinnedPnpm)) }
    return $null
}

function Test-WebTools { (Get-NodeMajor) -ge 20 -and [bool](Get-PnpmCommand) }

# ---- setup / doctor -----------------------------------------------------------

function Invoke-Doctor([switch] $Strict) {
    $missing = $false

    Write-Sec "Build - needed for make build / run / dist"
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if (Test-Path (Join-Path $cargoBin "rustup.exe")) {
        $hostTriple = (& (Join-Path $cargoBin "rustup.exe") show 2>$null | Select-String "Default host:").ToString() -replace ".*:\s*", ""
        if ($hostTriple -like "*windows-gnu*") { Write-Ok "rustup     host $hostTriple" }
        else {
            Write-Bad "rustup     host is $hostTriple; the GNU host builds without Visual Studio:"
            Write-Host "               rustup set default-host x86_64-pc-windows-gnu"
            $missing = $true
        }
    } else {
        Write-Bad "rustup     install per user (no admin):"
        Write-Host "               winget install --id Rustlang.Rustup   (then: rustup set default-host x86_64-pc-windows-gnu)"
        $missing = $true
    }
    $mingw = Find-MinGW
    if ($mingw) {
        $gccVersion = (& (Join-Path $mingw "gcc.exe") -dumpfullversion)
        Write-Ok "gcc        $gccVersion  ($mingw)"
        foreach ($tool in @("cmake.exe", "windres.exe", "mingw32-make.exe")) {
            if (Test-Path (Join-Path $mingw $tool)) { Write-Ok "$($tool.Replace('.exe','').PadRight(10)) with WinLibs" }
            else { Write-Warn "$($tool.Replace('.exe','').PadRight(10)) not in $mingw" }
        }
    } else {
        Write-Bad "gcc        WinLibs GCC (POSIX threads, MSVCRT) - unzip it to ~\tools\mingw64 or set OO_MINGW:"
        Write-Host "               winget install --id BrechtSanders.WinLibs.POSIX.MSVCRT"
        Write-Host "               (or download from https://winlibs.com)"
        $missing = $true
    }

    Write-Sec "Required - the app cannot run without these"
    if (Test-Cmd git) { Write-Ok "git        $((& git --version) -replace 'git version ', '')" }
    else { Write-Bad "git        winget install --id Git.Git"; $missing = $true }
    $bash = Get-GitBash
    if ($bash) { Write-Ok "git bash   $bash  (agent launches, hooks and A2A scripts run here)" }
    else { Write-Bad "git bash   comes with Git for Windows: winget install --id Git.Git"; $missing = $true }
    $wv2 = Get-WebView2Version
    if ($wv2) { Write-Ok "WebView2   $wv2  (browser tabs)" }
    else { Write-Warn "WebView2   runtime not found - browser tabs won't open: winget install --id Microsoft.EdgeWebView2Runtime" }

    Write-Sec "Agents - needed to run agents in worktree terminals (else the terminal is a plain shell)"
    if (Test-Cmd node) { Write-Ok "node       $(& node --version)" } else { Write-Warn "node       winget install --id OpenJS.NodeJS.LTS" }
    foreach ($agent in @("claude", "codex", "pi", "opencode", "grok")) {
        $c = Get-Command $agent -ErrorAction SilentlyContinue
        if ($c) { Write-Ok "$($agent.PadRight(10)) $($c.Source)" }
    }
    if (-not (Test-Cmd claude) -and -not (Test-Cmd codex) -and -not (Test-Cmd pi)) {
        Write-Warn "agent CLI  none found: npm install -g @anthropic-ai/claude-code  (or codex, pi ...)"
    }
    Write-Ok "tmux       none needed: the app's own oo-session.exe keeps agents running across tabs and restarts"

    Write-Sec "Remote web client - needed for make web / phone access (optional)"
    $major = Get-NodeMajor
    if ($major -ge 20) { Write-Ok "node       v$major (>= 20)" }
    elseif ($major -gt 0) { Write-Warn "node       v$major is too old, need >= 20: winget upgrade --id OpenJS.NodeJS.LTS" }
    else { Write-Warn "node       need >= 20: winget install --id OpenJS.NodeJS.LTS" }
    if (Test-Cmd pnpm) { Write-Ok "pnpm       $(& pnpm --version)" }
    elseif (Test-Cmd corepack) { Write-Ok "pnpm       through corepack" }
    elseif (Test-Cmd npx.cmd) { Write-Ok "pnpm       through npx ($(Get-PinnedPnpm))" }
    else { Write-Warn "pnpm       npm install -g pnpm" }

    Write-Sec "Optional"
    if (Test-Cmd gh) { Write-Ok "gh         $((& gh --version | Select-Object -First 1))" }
    else { Write-Warn "gh         PR + CI badges: winget install --id GitHub.cli  (then: gh auth login)" }
    $ra = Get-Command rust-analyzer -ErrorAction SilentlyContinue
    if ($ra -and (& $ra.Source --version 2>$null)) { Write-Ok "rust-analyzer $(& $ra.Source --version)" }
    else { Write-Warn "rust-analyzer  LSP in the editor: rustup component add rust-analyzer" }

    Write-Host ""
    if ($missing) {
        Write-Host "A required tool is missing. Install it above, then re-run: .\make setup" -ForegroundColor Red
        if ($Strict) { exit 1 }
    } else {
        Write-Host "All required tools present." -ForegroundColor Green
    }
}

# ---- build targets -------------------------------------------------------------

function Invoke-Web {
    if ((Get-NodeMajor) -lt 20) { throw "make web needs node >= 20 (found $(if (Test-Cmd node) { & node --version } else { 'none' }))" }
    $web = Join-Path $Root "web\remote"
    $pnpm = Get-PnpmCommand
    if (-not $pnpm) { throw "make web needs pnpm: npm install -g pnpm" }
    foreach ($step in @(@("install", "--frozen-lockfile"), @("build"))) {
        Invoke-Native $pnpm[0] (@($pnpm | Select-Object -Skip 1) + @("-C", $web) + $step)
    }
}

# The web client is embedded by crates/remote; without node >= 20 the app
# still builds, minus the phone client.
function Invoke-WebIfPossible {
    if (Test-WebTools) { Invoke-Web }
    else { Write-Warn "skipping the phone web client (needs node >= 20 and pnpm): Remote access won't serve a UI" }
}

# gpui_windows' release build precompiles its shaders with the Windows SDK's
# fxc.exe. Without the SDK, script/windows/fxc-shim stands in: the same
# command line on Windows' own d3dcompiler_47.dll.
function Use-Fxc {
    if ($env:GPUI_FXC_PATH -and (Test-Path $env:GPUI_FXC_PATH)) { return }
    if (Get-Command fxc.exe -ErrorAction SilentlyContinue) { return }
    $sdk = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\fxc.exe" -ErrorAction SilentlyContinue
    if ($sdk) { return }
    $shimTarget = Join-Path $App "target\fxc-shim"
    Invoke-Native cargo @("build", "--release", "--quiet", "--manifest-path",
        (Join-Path $Root "script\windows\fxc-shim\Cargo.toml"), "--target-dir", $shimTarget)
    $env:GPUI_FXC_PATH = Join-Path $shimTarget "release\fxc.exe"
    Write-Host "no Windows SDK fxc.exe; compiling gpui's shaders with $env:GPUI_FXC_PATH"
}

# Windows Terminal's ConPTY (conpty.dll + OpenConsole.exe, Microsoft's MIT
# NuGet package). The terminal library loads it from beside the exe instead
# of the ConPTY built into Windows, which hands console-API programs
# (Codex, PowerShell's line editor) arrow keys and mouse reports as text
# ("[A", "[<35;…M"). Downloaded once into target\conpty\<version>\pkg, its
# Microsoft signature checked; build.rs copies it next to the exe.
$ConPtyVersion = "1.25.261109001"
function Use-ConPty {
    $pkg = Join-Path $App "target\conpty\$ConPtyVersion\pkg"
    $dll = Join-Path $pkg "runtimes\win-x64\native\conpty.dll"
    if (Test-Path $dll) { return }
    $dir = Split-Path $pkg
    New-Item -ItemType Directory -Force $dir | Out-Null
    $zip = Join-Path $dir "conpty.zip"
    $url = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$ConPtyVersion/microsoft.windows.console.conpty.$ConPtyVersion.nupkg"
    Write-Host "fetching Windows Terminal's ConPTY $ConPtyVersion from nuget.org"
    Invoke-WebRequest $url -OutFile $zip
    Expand-Archive $zip -DestinationPath $pkg -Force
    foreach ($f in @($dll, (Join-Path $pkg "build\native\runtimes\x64\OpenConsole.exe"))) {
        $sig = Get-AuthenticodeSignature $f
        if ($sig.Status -ne "Valid" -or $sig.SignerCertificate.Subject -notmatch "O=Microsoft Corporation") {
            Remove-Item -Recurse -Force $pkg
            throw "ConPTY package file $f is not validly signed by Microsoft"
        }
    }
}

function Invoke-Build([string] $BuildProfile = "debug") {
    Use-Toolchain
    Invoke-WebIfPossible
    Use-ConPty
    if ($BuildProfile -eq "release") { Use-Fxc }
    Push-Location $App
    try {
        if ($BuildProfile -eq "release") { Invoke-Native cargo @("build", "--release") }
        else { Invoke-Native cargo @("build") }
    } finally { Pop-Location }
}

function Invoke-Run {
    Invoke-Build
    $exe = Join-Path $App "target\debug\harness-app.exe"
    $appArgs = if ($Rest) { $Rest } else { @($Root) }
    & $exe @appArgs
}

function Invoke-Check {
    Use-Toolchain
    Push-Location $App
    try { Invoke-Native cargo @("check") } finally { Pop-Location }
}

function Invoke-Test {
    Use-Toolchain
    # As in CI and the app: SHELL is Git Bash, and some tests spawn Unix tools
    # (sleep, cat) that Windows lacks. Git's usr\bin goes last on PATH so it
    # never shadows Windows' own commands.
    $bash = Get-GitBash
    if ($bash) {
        $env:SHELL = $bash
        $env:PATH = "$env:PATH;$(Join-Path (Split-Path (Split-Path $bash)) 'usr\bin')"
    } else {
        Write-Warn "Git Bash not found: tests that need sh, sleep or cat will fail"
    }
    Push-Location $Root
    # Every failure, not just the first crate's, as CI reports them.
    try { Invoke-Native cargo @("test", "--no-fail-fast") } finally { Pop-Location }
}

# A multi-size .ico (PNG-compressed entries, Vista+) from the 1024px PNG.
function Invoke-Icon {
    Add-Type -AssemblyName System.Drawing
    $src = [System.Drawing.Image]::FromFile((Join-Path $Root "assets\openOrchestra-1024.png"))
    $sizes = 256, 64, 48, 32, 24, 16
    $images = foreach ($s in $sizes) {
        $bmp = New-Object System.Drawing.Bitmap $s, $s
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $g.DrawImage($src, 0, 0, $s, $s)
        $g.Dispose()
        $ms = New-Object System.IO.MemoryStream
        $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
        , $ms.ToArray()
    }
    $src.Dispose()
    $out = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter $out
    $w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]$sizes.Count)
    $offset = 6 + 16 * $sizes.Count
    for ($i = 0; $i -lt $sizes.Count; $i++) {
        $s = $sizes[$i]; $len = $images[$i].Length
        $w.Write([byte]($s % 256)); $w.Write([byte]($s % 256))
        $w.Write([byte]0); $w.Write([byte]0)
        $w.Write([UInt16]1); $w.Write([UInt16]32)
        $w.Write([UInt32]$len); $w.Write([UInt32]$offset)
        $offset += $len
    }
    foreach ($img in $images) { $w.Write($img) }
    $w.Flush()
    $ico = Join-Path $Root "assets\openOrchestra.ico"
    [System.IO.File]::WriteAllBytes($ico, $out.ToArray())
    Write-Host "wrote $ico"
}

$DistName = "openOrchestra-windows-x64"

# The release build as a folder (and zip) that runs from anywhere: the exe,
# WebView2's loader, and the bundled sounds (also compiled in; kept beside
# it for parity with the macOS bundle).
function Invoke-Dist {
    Invoke-Build "release"
    $out = Join-Path $Root "dist\$DistName"
    if (Test-Path $out) { Remove-Item -Recurse -Force $out }
    New-Item -ItemType Directory -Force (Join-Path $out "sounds") | Out-Null
    $bin = Join-Path $App "target\release"
    Copy-Item (Join-Path $bin "harness-app.exe") (Join-Path $out "openOrchestra.exe")
    # The GNU toolchain keeps the symbol table (~300 MB); the shipped copy
    # drops it (WinLibs' strip), the build's own keeps it for debugging.
    $strip = Join-Path (Find-MinGW) "strip.exe"
    if (Test-Path $strip) { Invoke-Native $strip @("--strip-all", (Join-Path $out "openOrchestra.exe")) }
    # The tmux stand-in agent sessions run in; the app looks for it beside itself.
    Copy-Item (Join-Path $bin "oo-session.exe") $out
    if (Test-Path $strip) { Invoke-Native $strip @("--strip-all", (Join-Path $out "oo-session.exe")) }
    Copy-Item (Join-Path $bin "WebView2Loader.dll") $out
    # Windows Terminal's ConPTY (see Use-ConPty), which build.rs put there.
    Copy-Item (Join-Path $bin "conpty.dll"), (Join-Path $bin "OpenConsole.exe") $out
    Copy-Item (Join-Path $App "sounds\*.wav") (Join-Path $out "sounds")
    Copy-Item (Join-Path $Root "LICENSE*") $out -ErrorAction SilentlyContinue
    $zip = Join-Path $Root "dist\$DistName.zip"
    if (Test-Path $zip) { Remove-Item -Force $zip }
    Compress-Archive -Path (Join-Path $out "*") -DestinationPath $zip
    Write-Host "dist: $out"
    Write-Host "      $zip"
}

$InstallDir = Join-Path $env:LOCALAPPDATA "Programs\openOrchestra"
$Shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\openOrchestra.lnk"
# The app's toast registration (notifier_win.rs); install's shortcut gets the
# app id and activator from the app's first run.
$AumidKey = "HKCU:\Software\Classes\AppUserModelId\openOrchestra.Harness"
$ActivatorKey = "HKCU:\Software\Classes\CLSID\{c6d5773d-967c-4327-bee3-fa480725b2c4}"

function Invoke-Install {
    Invoke-Dist
    Get-Process openOrchestra -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$InstallDir*" } | Stop-Process
    New-Item -ItemType Directory -Force $InstallDir | Out-Null
    Copy-Item -Recurse -Force (Join-Path $Root "dist\$DistName\*") $InstallDir
    $exe = Join-Path $InstallDir "openOrchestra.exe"
    # Replaces a dev build's shortcut: the installed app is the one toasts
    # and the Start menu should open.
    if (Test-Path $Shortcut) { Remove-Item -Force $Shortcut }
    $shell = New-Object -ComObject WScript.Shell
    $lnk = $shell.CreateShortcut($Shortcut)
    $lnk.TargetPath = $exe
    $lnk.WorkingDirectory = $env:USERPROFILE
    $lnk.Description = "Run coding agents in parallel git worktrees"
    $lnk.Save()
    Write-Host "installed: $exe"
    Write-Host "Start menu: openOrchestra (its first run finishes the notification setup)"
}

function Invoke-Uninstall {
    Get-Process openOrchestra -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$InstallDir*" } | Stop-Process
    foreach ($p in @($InstallDir, $Shortcut, $AumidKey, $ActivatorKey)) {
        if (Test-Path $p) { Remove-Item -Recurse -Force $p; Write-Host "removed $p" }
    }
    Write-Host "App data is kept: ~\.openorchestra and %LOCALAPPDATA%\openOrchestra"
}

switch ($Target) {
    "setup"     { Invoke-Doctor -Strict }
    "doctor"    { Invoke-Doctor }
    "build"     { Invoke-Build }
    "run"       { Invoke-Run }
    "check"     { Invoke-Check }
    "test"      { Invoke-Test }
    "web"       { Invoke-Web }
    "icon"      { Invoke-Icon }
    "dist"      { Invoke-Dist }
    "install"   { Invoke-Install }
    "uninstall" { Invoke-Uninstall }
    { $_ -in "app", "dmg", "dmg-notarized", "release", "dmg-background" } {
        Write-Host "make $Target packages the macOS app; on Windows use: make dist / make install" -ForegroundColor Yellow
        exit 1
    }
    default     { Get-Help $PSCommandPath -Detailed | Out-String | Write-Host }
}
