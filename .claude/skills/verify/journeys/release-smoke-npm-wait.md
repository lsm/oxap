# Journey: the release smoke waits for npm to show a fresh publish

npm accepts a publish and then says the package "may take a few minutes to
become available", so `npm view` can miss a version that `publish-npm` has
just reported done. The `npm` job in `release-smoke.yml` therefore checks again
every 30 seconds within a hard budget: 600 seconds on a release run, 60 on a
pull request. Each `npm view` runs under `timeout` with the budget that is left,
and each pause is cut to it, so a slow registry cannot carry the check past its
deadline.

Run the step's own shell from the workflow file against a fake `npm` and a
`timeout` shim:

```sh
python3 .claude/skills/verify/scripts/release-smoke-npm-wait.py; echo "exit=$?"
```

It must print four `PASS` lines, `failed=0` and `exit=0`. Each case must also
finish within its budget plus 1.5 seconds:

- a version that appears on the third pass sets `published=true` and exits 0;
- a version that never appears fails the release run with `published=false`;
- on a pull request a missing version only warns;
- a registry that takes 5 seconds per lookup fails a 2-second budget in about 2
  seconds, not after four full lookups.

Pass another workflow file as the first argument to test it instead. The
`main` version before this check fails the first case with exit 1 after four
`npm view` calls, which is the alpha.38 release failure.

On a real release, the run's `Release smoke / Published npm package` job must be
green on its first attempt.
