[CmdletBinding()]
param(
    [Parameter(Mandatory)] [ValidateSet('Prepare', 'Publish')] [string] $Mode,
    [Parameter(Mandatory)] [ValidatePattern('^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$')] [string] $Version,
    [Parameter(Mandatory)] [string] $Tool,
    [Parameter(Mandatory)] [string] $Dist,
    [string] $Notes
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$repo = 'LoonEman1/voxhold-native-rust'
$tag = "v$Version"
$baseUrl = "https://github.com/$repo/releases/download"
$channelAssets = @('latest.json.sig', 'latest.json')
$packageAssets = @('VoxHold-portable.zip', 'VoxHold-Setup.exe')
$scratch = New-Item -ItemType Directory -Path (Join-Path $env:RUNNER_TEMP "voxhold-$Mode-$([guid]::NewGuid())")

function Get-Releases {
    gh api "repos/$repo/releases?per_page=100" --paginate --jq '.[] | {id, tag_name, draft, target_commitish, assets}' |
        ForEach-Object { $_ | ConvertFrom-Json }
}

function Assert-ReleaseAssets($Release, [string[]] $Files) {
    foreach ($file in $Files) {
        $name = Split-Path -Leaf $file
        $digest = 'sha256:' + (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
        $remote = $Release.assets | Where-Object name -CEQ $name
        if ($remote.state -ne 'uploaded' -or $remote.digest -cne $digest) {
            throw "GitHub asset $($Release.tag_name)/$name did not match the verified build."
        }
    }
}

function Save-Release([string] $Tag, [string] $Directory, [string[]] $Names) {
    $patterns = foreach ($name in $Names) { '--pattern'; $name }
    gh release download $Tag --repo $repo --dir $Directory @patterns
}

function Read-SignedRelease([string] $Directory, [string] $Tag) {
    & $Tool verify --manifest (Join-Path $Directory 'latest.json') --sig (Join-Path $Directory 'latest.json.sig') | Out-Host
    $manifest = Get-Content -LiteralPath (Join-Path $Directory 'latest.json') -Raw | ConvertFrom-Json
    if ($manifest.channel -cne 'beta' -or "v$($manifest.version)" -cne $Tag -or
        $manifest.full.url -cne "$baseUrl/$Tag/VoxHold-portable.zip" -or
        $manifest.setup.url -cne "$baseUrl/$Tag/VoxHold-Setup.exe") {
        throw "The signed manifest does not describe beta release $Tag."
    }
    foreach ($delta in $manifest.deltas) {
        if ($delta.url -cne "$baseUrl/$Tag/VoxHold-delta-$($delta.from_version).zip") {
            throw "A delta URL points outside release $Tag."
        }
    }
    return $manifest
}

$releases = @(Get-Releases)
$published = @($releases | Where-Object { !$_.draft -and $_.tag_name -cmatch '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' } |
    Sort-Object { [version]$_.tag_name.Substring(1) } -Descending)
if ($published.Count -gt 0 -and [version]$published[0].tag_name.Substring(1) -gt [version]$Version) {
    throw "Refusing to replace $($published[0].tag_name) with the older version $tag."
}

if ($Mode -eq 'Prepare') {
    $previous = $published | Where-Object tag_name -CNE $tag | Select-Object -First 1
    if ($previous) {
        $previousTag = $previous.tag_name
        Save-Release $previousTag $scratch.FullName ($channelAssets + $packageAssets[0])
        $null = Read-SignedRelease $scratch.FullName $previousTag
        $base = Join-Path $scratch.FullName $packageAssets[0]
        & $Tool verify --manifest (Join-Path $scratch.FullName 'latest.json') `
            --sig (Join-Path $scratch.FullName 'latest.json.sig') --full $base
        $delta = Join-Path $Dist "VoxHold-delta-$($previousTag.Substring(1)).zip"
        # Delta generation can decline a delta that is not smaller than the full ZIP.
        $PSNativeCommandUseErrorActionPreference = $false
        & $Tool delta --base $base --target (Join-Path $Dist $packageAssets[0]) --out $delta
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "Using only the full package for $tag."
            if (Test-Path -LiteralPath $delta -PathType Leaf) { Remove-Item -LiteralPath $delta }
        }
    }
    exit 0
}

$commit = git rev-parse HEAD
$release = $releases | Where-Object tag_name -CEQ $tag
if (!$release -or $release.draft) {
    if ((git tag --list $tag) -and (git rev-list -n 1 "refs/tags/$tag") -cne $commit) {
        throw "$tag points to a different commit. Increase the version to publish a new build."
    }
    if ($release -and $release.target_commitish -cne $commit) {
        throw "Draft $tag was built from a different commit."
    }
    $manifest = Read-SignedRelease $Dist $tag
    $names = $packageAssets + $channelAssets + @($manifest.deltas | ForEach-Object { "VoxHold-delta-$($_.from_version).zip" })
    $files = $names | ForEach-Object { Join-Path $Dist $_ }
    if (!$release) {
        gh release create $tag --repo $repo --target $commit --draft --prerelease --title "VoxHold $Version" --notes-file $Notes
    }
    gh release upload $tag @files --repo $repo --clobber
    $release = Get-Releases | Where-Object tag_name -CEQ $tag
    # Remove a delta left by a previous attempt only while this release is a draft.
    foreach ($asset in $release.assets | Where-Object { $_.name -like 'VoxHold-delta-*.zip' -and $_.name -cnotin $names }) {
        gh release delete-asset $tag $asset.name --repo $repo --yes
    }
    Assert-ReleaseAssets $release $files
    gh release edit $tag --repo $repo --draft=false
    $signedDir = $Dist
} else {
    # A retry may find the version already public. Keep its original signed bytes.
    $signedDir = (New-Item -ItemType Directory -Path (Join-Path $scratch.FullName 'published')).FullName
    Save-Release $tag $signedDir $channelAssets
    $manifest = Read-SignedRelease $signedDir $tag
    $expected = @{
        'VoxHold-portable.zip' = $manifest.full.sha256
        'VoxHold-Setup.exe' = $manifest.setup.sha256
    }
    foreach ($delta in $manifest.deltas) { $expected["VoxHold-delta-$($delta.from_version).zip"] = $delta.sha256 }
    foreach ($name in $expected.Keys) {
        $asset = $release.assets | Where-Object name -CEQ $name
        if ($asset.state -ne 'uploaded' -or $asset.digest -cne "sha256:$($expected[$name])") {
            throw "Published release $tag is missing the signed bytes of $name."
        }
    }
}

$channel = $releases | Where-Object tag_name -CEQ 'beta'
$body = "Current Windows beta: [VoxHold $Version](https://github.com/$repo/releases/tag/$tag). Download Setup or portable ZIP there."
$previousDir = $null
if (!$channel) {
    gh release create beta --repo $repo --target $commit --draft --prerelease --title 'VoxHold beta channel' --notes $body
} elseif (!$channel.draft -and @($channel.assets | Where-Object name -CIn $channelAssets).Count -eq $channelAssets.Count) {
    $previousDir = (New-Item -ItemType Directory -Path (Join-Path $scratch.FullName 'previous-channel')).FullName
    Save-Release beta $previousDir $channelAssets
    $changed = $channelAssets | Where-Object {
        (Get-FileHash -LiteralPath (Join-Path $previousDir $_)).Hash -ne (Get-FileHash -LiteralPath (Join-Path $signedDir $_)).Hash
    }
    if (!$changed) {
        Write-Host "$tag is already on the beta channel."
        exit 0
    }
    try {
        & $Tool verify --manifest (Join-Path $previousDir 'latest.json') --sig (Join-Path $previousDir 'latest.json.sig')
    } catch {
        $previousDir = $null
        Write-Warning 'The existing channel files do not form a signed pair and cannot be used for restoration.'
    }
}

try {
    # Keep older clients on their existing URLs. The versioned packages are already public.
    foreach ($name in $channelAssets) {
        gh release upload beta (Join-Path $signedDir $name) --repo $repo --clobber
    }
    $remote = Get-Releases | Where-Object tag_name -CEQ 'beta'
    $channelFiles = $channelAssets | ForEach-Object { Join-Path $signedDir $_ }
    Assert-ReleaseAssets $remote $channelFiles
    gh release edit beta --repo $repo --notes $body --draft=false
} catch {
    if ($previousDir) {
        Write-Warning 'Channel update failed. Restoring the previous manifest and signature.'
        foreach ($name in $channelAssets) {
            gh release upload beta (Join-Path $previousDir $name) --repo $repo --clobber
        }
    }
    throw
}
