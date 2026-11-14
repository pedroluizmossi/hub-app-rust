# Installs (or updates) MiniUtils on Windows, from the latest Release on GitHub.
#
#   irm https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.ps1 | iex
#
# For the current user only, no administrator:
#   %LOCALAPPDATA%\Programs\MiniUtils\miniutils.exe     the program (and README and licenses)
#   Start menu > MiniUtils                               shortcut
#
# Uninstall (the configuration stays):
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.ps1))) -Uninstall
#
# A specific version: $env:MINIUTILS_VERSION = "0.2.0" before the command.
#
# Everything stays inside functions and errors go out through `throw`: run through `iex`,
# an `exit` would close the user's PowerShell window.

param([switch]$Uninstall)

# The preferences stay inside the functions: through `iex`, at the top they would change
# the user's session.
function Install-MiniUtils {
    $ErrorActionPreference = "Stop"
    $ProgressPreference = "SilentlyContinue" # Invoke-WebRequest's progress bar makes the download slow
    $repo = "pedroluizmossi/hub-app-rust"
    $dir = Join-Path $env:LOCALAPPDATA "Programs\MiniUtils"
    $exe = Join-Path $dir "miniutils.exe"
    $shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "MiniUtils.lnk"

    if (-not [Environment]::Is64BitOperatingSystem) {
        throw "there is only a package for 64-bit Windows"
    }
    # Windows PowerShell 5.1 may not turn on TLS 1.2 by itself, and GitHub requires it
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    if ($env:MINIUTILS_VERSION) {
        $version = $env:MINIUTILS_VERSION.TrimStart("v")
    } else {
        $latest = Invoke-RestMethod -UseBasicParsing "https://api.github.com/repos/$repo/releases/latest"
        $version = $latest.tag_name.TrimStart("v")
    }

    $name = "miniutils-$version-windows-x86_64"
    $archive = "$name.zip"
    $base = "https://github.com/$repo/releases/download/v$version"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("miniutils-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null

    try {
        Write-Host "Downloading MiniUtils $version..."
        $zip = Join-Path $tmp $archive
        Invoke-WebRequest -UseBasicParsing "$base/$archive" -OutFile $zip
        $sums = (Invoke-WebRequest -UseBasicParsing "$base/$archive.sha256").Content
        if ($sums -is [byte[]]) { $sums = [Text.Encoding]::ASCII.GetString($sums) }
        $expected = ($sums.Trim() -split "\s+")[0]
        $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash
        if ($actual -ne $expected) {
            throw "the SHA-256 sum does not match: the downloaded file is corrupt or was altered"
        }
        Expand-Archive -Path $zip -DestinationPath $tmp -Force

        # a running .exe cannot be replaced: close it and open it again at the end
        $running = @(Get-Process -Name miniutils -ErrorAction SilentlyContinue)
        if ($running.Count -gt 0) {
            Write-Host "Closing the running MiniUtils to update it..."
            $running | Stop-Process -Force
            $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
        }

        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        Copy-Item -Path (Join-Path $tmp "$name\*") -Destination $dir -Recurse -Force

        $shell = New-Object -ComObject WScript.Shell
        $link = $shell.CreateShortcut($shortcut)
        $link.TargetPath = $exe
        $link.WorkingDirectory = $dir
        $link.Description = "Mini-app hub in the tray"
        $link.Save()

        Write-Host "MiniUtils $version installed in $dir"
        Write-Host "Open it from the Start menu (MiniUtils)."
        if ($running.Count -gt 0) {
            Start-Process -FilePath $exe -ArgumentList "--hidden"
            Write-Host "MiniUtils was opened again in the tray."
        }
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

function Uninstall-MiniUtils {
    $ErrorActionPreference = "Stop"
    $dir = Join-Path $env:LOCALAPPDATA "Programs\MiniUtils"
    $shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "MiniUtils.lnk"

    Get-Process -Name miniutils -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
    Remove-Item -Force $shortcut -ErrorAction SilentlyContinue
    # "Start with the system" (src/autostart/windows.rs)
    Remove-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "MiniUtils" -ErrorAction SilentlyContinue
    Write-Host "MiniUtils removed. The configuration in $env:APPDATA\miniutils was not deleted."
}

if ($Uninstall) { Uninstall-MiniUtils } else { Install-MiniUtils }
