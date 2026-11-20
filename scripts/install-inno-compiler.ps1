[CmdletBinding()]
param([Parameter(Mandatory)] [string] $Directory)

$ErrorActionPreference = 'Stop'
$compiler = 'innosetup-6.7.3.exe'
$sha256 = '9c73c3bae7ed48d44112a0f48e66742c00090bdb5bef71d9d3c056c66e97b732'
$Directory = (New-Item -ItemType Directory -Path $Directory -Force).FullName
$download = Join-Path $Directory $compiler
$destination = Join-Path $Directory 'compiler'
$iscc = Join-Path $destination 'ISCC.exe'
Invoke-WebRequest -Uri "https://github.com/jrsoftware/issrc/releases/download/is-6_7_3/$compiler" -OutFile $download -MaximumRetryCount 3
if ((Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash -ine $sha256) {
    throw 'Inno Setup download does not match the pinned SHA-256.'
}
$arguments = "/PORTABLE=1 /CURRENTUSER /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /DIR=`"$destination`""
$process = Start-Process -FilePath $download -ArgumentList $arguments -WorkingDirectory $Directory -Wait -PassThru
if ($process.ExitCode -ne 0 -or !(Test-Path -LiteralPath $iscc -PathType Leaf)) {
    throw "Portable Inno Setup installation failed with exit code $($process.ExitCode)."
}
Write-Output $iscc
