# Zips one folder: diagnose.ps1's results, and pack.sh's bundle.
#
#   zip-folder.ps1 -Dir <folder> -Zip <file.zip> [-Strict]
#
# Entries are named with forward slashes. Windows PowerShell 5.1's Compress-Archive, and
# ZipFile.CreateFromDirectory under powershell.exe, store them with backslashes, which unzip tools
# outside Windows warn about or refuse.
#
# A file something else still has open is read anyway, and one that cannot be read at all is left
# out and named, one line of output each, instead of ending the zip: on one tester's PC a daemon
# log was still held when the zip was made, and CreateEntryFromFile, which refuses such a file,
# lost the whole zip. With -Strict a file left out is an error (exit 1), for a bundle that must be
# complete.

param(
    [Parameter(Mandatory = $true)][string]$Dir,
    [Parameter(Mandatory = $true)][string]$Zip,
    [switch]$Strict
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$Dir = (Resolve-Path $Dir).Path.TrimEnd('\')
if (Test-Path $Zip) { Remove-Item $Zip -Force }
$archive = [System.IO.Compression.ZipFile]::Open($Zip, [System.IO.Compression.ZipArchiveMode]::Create)
$unread = @()
try {
    $root = Split-Path $Dir -Parent
    Get-ChildItem -Path $Dir -Recurse -File | ForEach-Object {
        $name = $_.FullName.Substring($root.Length + 1).Replace('\', '/')
        try {
            $source = [System.IO.File]::Open($_.FullName, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read,
                ([System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete))
            try {
                $target = $archive.CreateEntry($name, [System.IO.Compression.CompressionLevel]::Optimal).Open()
                try { $source.CopyTo($target) } finally { $target.Dispose() }
            } finally {
                $source.Dispose()
            }
        } catch {
            $cause = $_.Exception
            if ($cause.InnerException) { $cause = $cause.InnerException }
            $unread += $cause.Message
        }
    }
} finally {
    $archive.Dispose()
}
$unread
if ($Strict -and $unread.Count -gt 0) { exit 1 }
