[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Tool,
    [Parameter(Mandatory)] [string] $Dist,
    [switch] $PortableOnly
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$Tool = (Resolve-Path -LiteralPath $Tool).Path
$Dist = (Resolve-Path -LiteralPath $Dist).Path
$root = (New-Item -ItemType Directory -Path (Join-Path ([IO.Path]::GetTempPath()) "VoxHold smoke $([guid]::NewGuid())")).FullName
$zip = Join-Path $Dist 'VoxHold-portable.zip'
$portable = Join-Path $root 'Portable'
$setup = Join-Path $root 'Setup'
$originalAppData = $env:APPDATA
$originalLocalAppData = $env:LOCALAPPDATA

function Assert-Payload([string] $Install, $Index) {
    foreach ($file in $Index.files) {
        $path = Join-Path $Install $file.path
        if ((Get-Item -LiteralPath $path).Length -ne $file.size -or
            (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $file.sha256) {
            throw "$Install has different bytes for $($file.path)."
        }
    }
}

function Test-Update([string] $Install, $Index) {
    $exe = Join-Path $Install 'Voxhold.exe'
    $staging = Join-Path $root "staging-$(Split-Path -Leaf $Install)"
    $handshake = Join-Path $staging 'handshake.ok'
    $log = Join-Path $staging 'apply.log'
    $userFile = Join-Path $Install 'smoke-user-state.txt'
    $oldAsset = Join-Path $Install 'obsolete-smoke-asset.txt'
    $startMarker = Join-Path $root 'startup.ok'
    Remove-Item -LiteralPath $startMarker -ErrorAction SilentlyContinue
    & $Tool unpack --zip $zip --out (Join-Path $staging 'app')

    # Model an older installation with an obsolete tracked asset and user data.
    'user state' | Set-Content -LiteralPath $userFile
    'old asset' | Set-Content -LiteralPath $oldAsset
    $oldIndex = @{
        version = '0.0.0'
        files = @($Index.files) + @(@{
            path = (Split-Path -Leaf $oldAsset)
            size = (Get-Item -LiteralPath $oldAsset).Length
            sha256 = (Get-FileHash -LiteralPath $oldAsset).Hash.ToLowerInvariant()
        })
    }
    $oldIndex | ConvertTo-Json -Depth 5 -Compress | Set-Content -LiteralPath (Join-Path $Install 'package.json')

    $client = Start-Process -FilePath $exe -WorkingDirectory $root -ArgumentList "--update-handshake `"$startMarker`"" -PassThru
    try {
        $deadline = [DateTime]::UtcNow.AddSeconds(30)
        while (!(Test-Path -LiteralPath $startMarker)) {
            if ($client.HasExited -or [DateTime]::UtcNow -ge $deadline) { throw "Client did not open from $Install." }
            Start-Sleep -Milliseconds 200
        }

        $helper = Join-Path $staging 'app/voxhold-updater.exe'
        $arguments = "--pid $($client.Id) --target `"$Install`" --staged `"$staging`" --exe Voxhold.exe --handshake `"$handshake`""
        $launcher = Start-Process -FilePath $helper -ArgumentList $arguments -WorkingDirectory $root -PassThru
        if (!$launcher.WaitForExit(10000) -or $launcher.ExitCode -ne 0) { throw 'Failed to launch the update helper.' }
        if (!$client.CloseMainWindow() -or !$client.WaitForExit(10000)) { throw 'The old client did not close.' }

        $deadline = [DateTime]::UtcNow.AddSeconds(55)
        while ([DateTime]::UtcNow -lt $deadline) {
            $progress = if (Test-Path -LiteralPath $log) { Get-Content -LiteralPath $log -Raw }
            if ($progress -match 'apply succeeded backup_removed=true') { break }
            if ($progress -match 'apply failed|wait failed') { throw $progress }
            Start-Sleep -Milliseconds 200
        }
        if ($progress -notmatch 'apply succeeded backup_removed=true') { throw "Update did not finish. $progress" }
        Assert-Payload $Install $Index
        $updated = Get-Content -LiteralPath (Join-Path $Install 'package.json') -Raw | ConvertFrom-Json
        if ($updated.version -ne $Index.version -or (Test-Path -LiteralPath $oldAsset) -or
            (Get-Content -LiteralPath $userFile -Raw).Trim() -ne 'user state') {
            throw "Wrong version or file state after update at $Install."
        }
        Write-Host "Applied the update from $Install and received the restarted client's handshake."
    } finally {
        Get-Process -Name Voxhold -ErrorAction SilentlyContinue | Where-Object Path -EQ $exe | ForEach-Object {
            if (!$_.CloseMainWindow() -or !$_.WaitForExit(5000)) { $_ | Stop-Process }
        }
    }
}

try {
    $env:APPDATA = Join-Path $root 'Profile/Roaming'
    $env:LOCALAPPDATA = Join-Path $root 'Profile/Local'
    & $Tool unpack --zip $zip --out $portable
    $index = Get-Content -LiteralPath (Join-Path $portable 'package.json') -Raw | ConvertFrom-Json
    Test-Update $portable $index

    if (!$PortableOnly) {
        $installer = Join-Path $Dist 'VoxHold-Setup.exe'
        $setupLog = Join-Path $root 'setup.log'
        $arguments = "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER /NOICONS /DIR=`"$setup`" /LOG=`"$setupLog`""
        $process = Start-Process -FilePath $installer -ArgumentList $arguments -WorkingDirectory $root -Wait -PassThru
        if ($process.ExitCode -ne 0) { throw "Setup failed with exit code $($process.ExitCode)." }
        Assert-Payload $setup $index
        if ((Get-FileHash -LiteralPath (Join-Path $setup 'package.json')).Hash -ne
            (Get-FileHash -LiteralPath (Join-Path $portable 'package.json')).Hash) {
            throw 'Setup and portable package indexes differ.'
        }
        Test-Update $setup $index
    }
} finally {
    $uninstaller = Join-Path $setup 'unins000.exe'
    if (Test-Path -LiteralPath $uninstaller -PathType Leaf) {
        $cleanup = Start-Process -FilePath $uninstaller -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART' -WorkingDirectory $root -Wait -PassThru
        if ($cleanup.ExitCode -ne 0) { Write-Warning "Setup smoke uninstall returned $($cleanup.ExitCode)." }
    }
    $env:APPDATA = $originalAppData
    $env:LOCALAPPDATA = $originalLocalAppData
    Write-Host "Review smoke test files at $root."
}
