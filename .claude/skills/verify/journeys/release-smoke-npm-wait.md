# Journey: the release smoke waits for npm to show a fresh publish

npm accepts a publish and then says the package "may take a few minutes to
become available", so `npm view` can miss a version that `publish-npm` has
just reported done. The `npm` job in `release-smoke.yml` therefore checks again
every 30 seconds for up to 10 minutes on a release run, and checks once on a
pull request.

Run the step's own shell from the workflow file against a fake `npm`:

```sh
python3 .claude/skills/verify/scripts/release-smoke-npm-wait.py; echo "exit=$?"
```

It must print three `PASS` lines, `failed=0` and `exit=0`:

- a version that appears on the third pass sets `published=true` and exits 0;
- a version that never appears fails the release run with `published=false`;
- on a pull request it checks each package once and only warns.

Pass another workflow file as the first argument to test it instead. The
`main` version before this check fails the first case with exit 1 after four
`npm view` calls, which is the alpha.38 release failure.

On a real release, the run's `Release smoke / Published npm package` job must be
green on its first attempt.
