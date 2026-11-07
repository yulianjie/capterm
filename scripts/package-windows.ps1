#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?\z')]
    [string] $Version,

    [Parameter(Mandatory)]
    [string] $Executable,

    [string] $OutputDirectory = (Join-Path $PSScriptRoot '..\dist')
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-X64Executable([string] $Path) {
    $reader = [IO.BinaryReader]::new([IO.File]::OpenRead($Path))
    try {
        if ($reader.BaseStream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5A4D) {
            throw "Expected a Windows executable at $Path."
        }

        $reader.BaseStream.Position = 0x3C
        $headerOffset = $reader.ReadUInt32()
        if ($headerOffset -gt ($reader.BaseStream.Length - 24)) {
            throw "Invalid Windows executable header at $Path."
        }

        $reader.BaseStream.Position = $headerOffset
        if ($reader.ReadUInt32() -ne 0x4550 -or $reader.ReadUInt16() -ne 0x8664) {
            throw "Expected a Windows x64 executable at $Path."
        }
    } finally {
        $reader.Dispose()
    }
}

function Find-InnoSetupCompiler {
    if ($env:CAPTERM_ISCC) {
        return (Resolve-Path -LiteralPath $env:CAPTERM_ISCC).ProviderPath
    }

    $command = Get-Command ISCC.exe -CommandType Application -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $installRoots = @(${env:ProgramFiles(x86)}, $env:ProgramFiles, (Join-Path $env:LOCALAPPDATA 'Programs'))
    foreach ($root in $installRoots) {
        if ($root) {
            $compiler = Join-Path $root 'Inno Setup 6\ISCC.exe'
            if (Test-Path -LiteralPath $compiler -PathType Leaf) {
                return $compiler
            }
        }
    }

    throw 'Install Inno Setup 6.3 or later, add ISCC.exe to PATH, or set CAPTERM_ISCC to its full path.'
}

if (-not $IsWindows) {
    throw 'Windows packaging requires Windows.'
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$executablePath = (Resolve-Path -LiteralPath $Executable).ProviderPath
Assert-X64Executable $executablePath
$compiler = Find-InnoSetupCompiler

$fileVersion = ($Version -split '[-+]', 2)[0]
$versionParts = [version] $fileVersion
if ($versionParts.Major -gt 65535 -or $versionParts.Minor -gt 65535 -or $versionParts.Build -gt 65535) {
    throw 'Version components must fit into the Windows file version (0–65535).'
}

$outputPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
$outputPath = [IO.Directory]::CreateDirectory($outputPath).FullName
$staging = [IO.Directory]::CreateDirectory((Join-Path $outputPath ".capterm-$([Guid]::NewGuid())"))
try {
    $payload = Join-Path $staging.FullName 'payload'
    $licenseDirectory = Join-Path $payload 'licenses'
    [void] [IO.Directory]::CreateDirectory($licenseDirectory)
    Copy-Item -LiteralPath $executablePath -Destination (Join-Path $payload 'capterm.exe')

    $licenses = [ordered] @{
        'assets/windows/conpty/LICENSE' = 'Microsoft-ConPTY-MIT.txt'
        'assets/fonts/MapleMono-OFL.txt' = 'Maple-Mono-OFL.txt'
        'assets/fonts/NotoSansSC-OFL.txt' = 'Noto-Sans-SC-OFL.txt'
    }
    foreach ($license in $licenses.GetEnumerator()) {
        Copy-Item -LiteralPath (Join-Path $repoRoot $license.Key) -Destination (Join-Path $licenseDirectory $license.Value)
    }

    $releaseName = "CapTerm-$Version-windows-x64"
    $installerName = "$releaseName-setup.exe"
    $portableName = "$releaseName-portable.zip"
    $installerScript = Join-Path $repoRoot 'packaging/windows/CapTerm.iss'
    & $compiler "/O$($staging.FullName)" "/F$releaseName-setup" "/DReleaseVersion=$Version" `
        "/DFileVersion=$fileVersion" "/DPayloadDirectory=$payload" $installerScript
    if ($LASTEXITCODE -ne 0) {
        throw "Inno Setup compilation failed with exit code $LASTEXITCODE."
    }

    [IO.Compression.ZipFile]::CreateFromDirectory($payload, (Join-Path $staging.FullName $portableName))

    $checksumName = 'SHA256SUMS.txt'
    $checksums = foreach ($name in @($installerName, $portableName)) {
        $hash = Get-FileHash -LiteralPath (Join-Path $staging.FullName $name) -Algorithm SHA256
        "{0}  {1}" -f $hash.Hash.ToLowerInvariant(), $name
    }
    [IO.File]::WriteAllText((Join-Path $staging.FullName $checksumName), (($checksums -join "`n") + "`n"))

    foreach ($name in @($installerName, $portableName, $checksumName)) {
        $artifact = Join-Path $outputPath $name
        [IO.File]::Move((Join-Path $staging.FullName $name), $artifact, $true)
        Write-Output $artifact
    }
} finally {
    $stagingPath = (Resolve-Path -LiteralPath $staging.FullName).ProviderPath
    if ((Split-Path -Parent $stagingPath) -ne $outputPath) {
        throw "Temporary packaging directory resolved outside the output directory: $stagingPath"
    }
    Remove-Item -LiteralPath $stagingPath -Recurse -Force
}
