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
        Assert-AssetDigest $Release $name $digest
    }
}

function Assert-AssetDigest($Release, [string] $Name, [string] $Digest) {
    $remote = $Release.assets | Where-Object name -CEQ $Name
    if ($remote.state -ne 'uploaded' -or $remote.digest -cne $Digest) {
        throw "GitHub asset $($Release.tag_name)/$Name did not match the verified build."
    }
}

function Get-ManifestArchiveNames([string] $Tag) {
    "manifest-$Tag.json.sig", "manifest-$Tag.json"
}

function Get-ChannelDeltaName([string] $FromVersion, [string] $ToVersion) {
    "VoxHold-delta-$FromVersion-to-$ToVersion.zip"
}

function Save-Release([string] $Tag, [string] $Directory, [string[]] $Names) {
    $patterns = foreach ($name in $Names) { '--pattern'; $name }
    gh release download $Tag --repo $repo --dir $Directory @patterns
}

function Save-PublishedManifest($Release, [string] $Directory) {
    $names = @(Get-ManifestArchiveNames $Release.tag_name)
    if (@($channel.assets | Where-Object name -CIn $names).Count -eq $names.Count) {
        Save-Release beta $Directory $names
        for ($index = 0; $index -lt $names.Count; $index++) {
            Move-Item -LiteralPath (Join-Path $Directory $names[$index]) -Destination (Join-Path $Directory $channelAssets[$index])
        }
    } else {
        # Releases published before the split carry their own signed pair.
        Save-Release $Release.tag_name $Directory $channelAssets
    }
}

function Read-SignedRelease([string] $Directory, [string] $Tag, [switch] $RequireBetaDeltas) {
    & $Tool verify --manifest (Join-Path $Directory 'latest.json') --sig (Join-Path $Directory 'latest.json.sig') | Out-Host
    $manifest = Get-Content -LiteralPath (Join-Path $Directory 'latest.json') -Raw | ConvertFrom-Json
    if ($manifest.channel -cne 'beta' -or "v$($manifest.version)" -cne $Tag -or
        $manifest.full.url -cne "$baseUrl/$Tag/VoxHold-portable.zip" -or
        $manifest.setup.url -cne "$baseUrl/$Tag/VoxHold-Setup.exe") {
        throw "The signed manifest does not describe beta release $Tag."
    }
    foreach ($delta in $manifest.deltas) {
        $newName = Get-ChannelDeltaName $delta.from_version $manifest.version
        $legacyUrl = "$baseUrl/$Tag/VoxHold-delta-$($delta.from_version).zip"
        if ($delta.url -cne "$baseUrl/beta/$newName" -and ($RequireBetaDeltas -or $delta.url -cne $legacyUrl)) {
            throw "Unexpected delta URL for $Tag."
        }
    }
    return $manifest
}

function Assert-PublishedPackages($Release, $Beta, $Manifest) {
    Assert-AssetDigest $Release 'VoxHold-portable.zip' "sha256:$($Manifest.full.sha256)"
    Assert-AssetDigest $Release 'VoxHold-Setup.exe' "sha256:$($Manifest.setup.sha256)"
    foreach ($delta in $Manifest.deltas) {
        if ($delta.url.StartsWith("$baseUrl/beta/", [StringComparison]::Ordinal)) {
            $name = Get-ChannelDeltaName $delta.from_version $Manifest.version
            Assert-AssetDigest $Beta $name "sha256:$($delta.sha256)"
        } else {
            Assert-AssetDigest $Release "VoxHold-delta-$($delta.from_version).zip" "sha256:$($delta.sha256)"
        }
    }
}

$releases = @(Get-Releases)
$channel = $releases | Where-Object tag_name -CEQ 'beta'
$published = @($releases | Where-Object { !$_.draft -and $_.tag_name -cmatch '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' } |
    Sort-Object { [version]$_.tag_name.Substring(1) } -Descending)
if ($published.Count -gt 0 -and [version]$published[0].tag_name.Substring(1) -gt [version]$Version) {
    throw "Refusing to replace $($published[0].tag_name) with the older version $tag."
}

if ($Mode -eq 'Prepare') {
    $previous = $published | Where-Object tag_name -CNE $tag | Select-Object -First 1
    if ($previous) {
        $previousTag = $previous.tag_name
        Save-PublishedManifest $previous $scratch.FullName
        $null = Read-SignedRelease $scratch.FullName $previousTag
        Save-Release $previousTag $scratch.FullName @($packageAssets[0])
        $base = Join-Path $scratch.FullName $packageAssets[0]
        & $Tool verify --manifest (Join-Path $scratch.FullName 'latest.json') `
            --sig (Join-Path $scratch.FullName 'latest.json.sig') --full $base
        $delta = Join-Path $Dist (Get-ChannelDeltaName $previousTag.Substring(1) $Version)
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
$newRelease = !$release -or $release.draft
if ($newRelease) {
    if ((git tag --list $tag) -and (git rev-list -n 1 "refs/tags/$tag") -cne $commit) {
        throw "$tag points to a different commit. Increase the version to publish a new build."
    }
    if ($release -and $release.target_commitish -cne $commit) {
        throw "Draft $tag was built from a different commit."
    }
    $signedDir = $Dist
    $manifest = Read-SignedRelease $signedDir $tag -RequireBetaDeltas
} else {
    # A retry may find the version already public. Keep its original signed bytes.
    $signedDir = (New-Item -ItemType Directory -Path (Join-Path $scratch.FullName 'published')).FullName
    Save-PublishedManifest $release $signedDir
    $manifest = Read-SignedRelease $signedDir $tag
    Assert-PublishedPackages $release $channel $manifest
}

$body = "Current Windows beta: [VoxHold $Version](https://github.com/$repo/releases/tag/$tag). Download Setup or portable ZIP there."
if (!$channel) {
    gh release create beta --repo $repo --target $commit --draft --prerelease --title 'VoxHold beta channel' --notes $body
}

if ($newRelease) {
    # Publish versioned update files before making the version public or advancing latest.
    $archiveNames = @(Get-ManifestArchiveNames $tag)
    $archiveFiles = @(for ($index = 0; $index -lt $archiveNames.Count; $index++) {
        $path = Join-Path $scratch.FullName $archiveNames[$index]
        Copy-Item -LiteralPath (Join-Path $signedDir $channelAssets[$index]) -Destination $path
        $path
    })
    $deltaFiles = @(foreach ($delta in $manifest.deltas) {
        Join-Path $Dist (Get-ChannelDeltaName $delta.from_version $Version)
    })
    $betaFiles = $archiveFiles + $deltaFiles
    gh release upload beta @betaFiles --repo $repo --clobber

    if (!$release) {
        gh release create $tag --repo $repo --target $commit --draft --prerelease --title "VoxHold $Version" --notes-file $Notes
    } else {
        # A draft left by the old publisher can still have update assets.
        foreach ($asset in $release.assets | Where-Object { $_.name -CIn $channelAssets -or $_.name -clike 'VoxHold-delta-*.zip' }) {
            gh release delete-asset $tag $asset.name --repo $repo --yes
        }
    }
    $files = $packageAssets | ForEach-Object { Join-Path $Dist $_ }
    gh release upload $tag @files --repo $repo --clobber
    $current = @(Get-Releases)
    $release = $current | Where-Object tag_name -CEQ $tag
    $beta = $current | Where-Object tag_name -CEQ 'beta'
    Assert-ReleaseAssets $beta $archiveFiles
    Assert-PublishedPackages $release $beta $manifest
    gh release edit $tag --repo $repo --draft=false
}

$previousDir = $null
if ($channel -and !$channel.draft -and @($channel.assets | Where-Object name -CIn $channelAssets).Count -eq $channelAssets.Count) {
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
    # Clients keep their existing discovery URLs. The signed package URLs are already available.
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
