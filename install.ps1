# Instala (ou atualiza) o MiniUtils no Windows, a partir do último Release no GitHub.
#
#   irm https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.ps1 | iex
#
# Só para o usuário, sem administrador:
#   %LOCALAPPDATA%\Programs\MiniUtils\miniutils.exe     o programa (e README e licenças)
#   Menu Iniciar > MiniUtils                             atalho
#
# Desinstalar (a configuração fica):
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.ps1))) -Uninstall
#
# Uma versão específica: $env:MINIUTILS_VERSION = "0.2.0" antes do comando.
#
# Tudo fica dentro de funções e os erros saem por `throw`: rodando por `iex`, um `exit`
# fecharia a janela do PowerShell do usuário.

param([switch]$Uninstall)

# As preferências ficam dentro das funções: por `iex`, no topo elas mudariam a sessão do
# usuário.
function Install-MiniUtils {
    $ErrorActionPreference = "Stop"
    $ProgressPreference = "SilentlyContinue" # a barra do Invoke-WebRequest deixa o download lento
    $repo = "pedroluizmossi/hub-app-rust"
    $dir = Join-Path $env:LOCALAPPDATA "Programs\MiniUtils"
    $exe = Join-Path $dir "miniutils.exe"
    $shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "MiniUtils.lnk"

    if (-not [Environment]::Is64BitOperatingSystem) {
        throw "só há pacote para Windows de 64 bits"
    }
    # Windows PowerShell 5.1 pode não ligar o TLS 1.2 sozinho, e o GitHub exige
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
        Write-Host "Baixando MiniUtils $version..."
        $zip = Join-Path $tmp $archive
        Invoke-WebRequest -UseBasicParsing "$base/$archive" -OutFile $zip
        $sums = (Invoke-WebRequest -UseBasicParsing "$base/$archive.sha256").Content
        if ($sums -is [byte[]]) { $sums = [Text.Encoding]::ASCII.GetString($sums) }
        $expected = ($sums.Trim() -split "\s+")[0]
        $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash
        if ($actual -ne $expected) {
            throw "a soma SHA-256 não bate: o arquivo baixado está corrompido ou foi alterado"
        }
        Expand-Archive -Path $zip -DestinationPath $tmp -Force

        # um .exe aberto não pode ser trocado: fecha e abre de novo no fim
        $running = @(Get-Process -Name miniutils -ErrorAction SilentlyContinue)
        if ($running.Count -gt 0) {
            Write-Host "Fechando o MiniUtils aberto para atualizar..."
            $running | Stop-Process -Force
            $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
        }

        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        Copy-Item -Path (Join-Path $tmp "$name\*") -Destination $dir -Recurse -Force

        $shell = New-Object -ComObject WScript.Shell
        $link = $shell.CreateShortcut($shortcut)
        $link.TargetPath = $exe
        $link.WorkingDirectory = $dir
        $link.Description = "Hub de mini-apps na bandeja"
        $link.Save()

        Write-Host "MiniUtils $version instalado em $dir"
        Write-Host "Abra pelo Menu Iniciar (MiniUtils)."
        if ($running.Count -gt 0) {
            Start-Process -FilePath $exe -ArgumentList "--hidden"
            Write-Host "O MiniUtils foi aberto de novo na bandeja."
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
    # "Iniciar com o sistema" (src/autostart/windows.rs)
    Remove-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "MiniUtils" -ErrorAction SilentlyContinue
    Write-Host "MiniUtils removido. A configuração em $env:APPDATA\miniutils não foi apagada."
}

if ($Uninstall) { Uninstall-MiniUtils } else { Install-MiniUtils }
