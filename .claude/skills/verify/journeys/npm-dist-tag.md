# Journey: a release lands on the npm dist-tag a plain install reads

`publish-npm` in `release-binaries.yml` asks `scripts/release-channels.mjs
dist-tag` which dist-tag to publish under. A prerelease takes `latest` until a
stable version other than the `0.0.0` name placeholder is on npm, then `next`.

Before tagging, ask the live registry what the tag run will choose:

```sh
node scripts/release-channels.mjs dist-tag --version <version>; echo "exit=$?"
```

While every `@oxap/cli` version is a prerelease or `0.0.0`, it must print
`latest` and exit 0.

After the tag run's `publish-npm` is green, every package must carry it:

```sh
for p in cli cli-darwin-arm64 cli-darwin-x64 cli-linux-arm64 cli-linux-x64 cli-win32-x64 cli-win32-arm64 sdk client; do
  printf '%s ' "@oxap/$p"; npm view "@oxap/$p" dist-tags.latest
done
```

Each line must end with the released version. Then install it where a user
would, with a fresh prefix and no project:

```sh
prefix="$(mktemp -d)"
npm i -g --prefix "$prefix" @oxap/cli; echo "exit=$?"
"$prefix/bin/oxap" --version; echo "exit=$?"
```

The version line must name the released version.
