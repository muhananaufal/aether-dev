# Packaging

Manifests for the package managers that do not need a submission: a Scoop
bucket and a Homebrew tap are both just repositories you own.

Neither file here is usable as it stands. Both need the SHA256 of archives that
only exist once `release.yml` has run for a tag, and both need updating for
every release afterwards.

## What to do after a release

`release.yml` publishes a `SHA256SUMS` file alongside the archives. Take the
sums from there rather than downloading and hashing by hand — the file is
produced on the runner that built the archive, so a mismatch means something
went wrong in between, which is exactly what you want to notice.

```
gh release download vX.Y.Z --pattern SHA256SUMS --output -
```

Then fill in `VERSION` and each `SHA256` placeholder in the templates below.

## Scoop (Windows)

A bucket is a repository with a `bucket/` directory of JSON manifests. Create
one called `scoop-bucket`, copy `adev.json` into `bucket/`, and it installs
with:

```
scoop bucket add muhananaufal https://github.com/muhananaufal/scoop-bucket
scoop install adev
```

## Homebrew (macOS and Linux)

A tap is a repository named `homebrew-<something>` with a `Formula/`
directory. Create one called `homebrew-tap`, copy `adev.rb` into `Formula/`,
and it installs with:

```
brew tap muhananaufal/tap
brew install adev
```

`homebrew-core` is a different thing: it has notability requirements and its
own review, and is worth considering only once the tap has users.

## Not here yet

WinGet needs a pull request to `microsoft/winget-pkgs`, and Chocolatey needs a
nuspec plus a moderation review for a new package. Both are worth doing when
somebody is actually asking for them; neither is worth maintaining before that.
