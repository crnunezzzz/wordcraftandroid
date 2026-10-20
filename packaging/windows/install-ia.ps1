<#
.SYNOPSIS
  Installs (or updates) WordCraft IA for the current user, beside the official WordCraft.

.DESCRIPTION
  Copies wordcraft.exe and wordcraft-cli.exe from this folder to
  %LOCALAPPDATA%\Programs\WordCraft IA and adds "WordCraft IA" to the Start menu and the desktop.
  No administrator rights are needed. The official WordCraft that the Craft Apps launcher installs
  and updates lives elsewhere, so its updates never replace this copy (nor this one that).
  Settings, recent files and API keys are shared with the official app and kept on update.

  Run it with a right click › "Run with PowerShell", or:
    powershell -ExecutionPolicy Bypass -File install-ia.ps1
#>
param([string] $Source = $PSScriptRoot)
$ErrorActionPreference = 'Stop'
$App = 'wordcraft'
$Title = 'WordCraft IA'
$Dest = Join-Path $env:LOCALAPPDATA "Programs\$Title"

foreach ($file in "$App.exe", "$App-cli.exe") {
  if (-not (Test-Path -LiteralPath (Join-Path $Source $file))) { throw "Falta $file junto a este instalador: descomprime todo el zip primero." }
}

# A copy of WordCraft IA that is running would keep its program file locked.
$running = Get-Process -Name $App -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($Dest, [StringComparison]::OrdinalIgnoreCase) }
foreach ($process in $running) {
  Write-Host "Cerrando $Title (guarda tus documentos si te lo pide)..."
  $null = $process.CloseMainWindow()
  if (-not $process.WaitForExit(30000)) { throw "Cierra $Title y vuelve a ejecutar el instalador." }
}

New-Item -ItemType Directory -Force -Path $Dest | Out-Null
foreach ($file in "$App.exe", "$App-cli.exe", 'LEEME-IA.txt') {
  $from = Join-Path $Source $file
  if (Test-Path -LiteralPath $from) {
    Copy-Item -LiteralPath $from -Destination $Dest -Force
    # Downloaded files carry the "from the internet" mark; this copy is the one the user chose.
    Unblock-File -LiteralPath (Join-Path $Dest $file) -ErrorAction SilentlyContinue
  }
}

$shell = New-Object -ComObject WScript.Shell
foreach ($folder in [Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop')) {
  $link = $shell.CreateShortcut((Join-Path $folder "$Title.lnk"))
  $link.TargetPath = Join-Path $Dest "$App.exe"
  $link.WorkingDirectory = $Dest
  $link.IconLocation = "$(Join-Path $Dest "$App.exe"),0"
  $link.Description = 'WordCraft con el asistente de IA (claves de API o tu plan de ChatGPT / Claude)'
  $link.Save()
}

Write-Host ""
Write-Host "$Title quedó instalado en $Dest."
Write-Host "Ábrelo desde el menú Inicio o el escritorio. El asistente está en la pestaña «Asistente» de la cinta."
