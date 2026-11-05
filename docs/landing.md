# Landing a change

A change merges into `main` as soon as it's ready, without waiting for CI. The checks that matter
most run first on your own machine, and CI runs everything on `main` after the merge. A red run on
`main` gets a fix or a revert straight away.

```
your branch -> the gate on your machine -> a pull request to main -> merged -> CI on main
```

## Landing

1. Merge the latest `main` into your branch, so that the gate tests what will be on `main`:

   ```sh
   git fetch origin
   git merge origin/main
   ```

2. Run the gate, with everything committed:

   ```sh
   .github/scripts/gate.sh
   ```

   It checks formatting, runs clippy, every test and every example and kit, and runs the JIT as
   x86-64 under Rosetta. It stops at the first failure and prints that step's output. A test program
   that runs more than a minute or holds more than 4 GB is stopped, and the gate says which. It takes
   four to five minutes when the build is warm.

3. Open a pull request to `main`, and say that the gate passed. The coordinator merges it.

## When CI on `main` fails

1. Find the failed job in the Actions tab, under the workflow "CI" or "CodeQL".
2. If your change caused it, open a fix straight away, or a revert of your change if the fix will
   take a while.
3. If a check fails only now and then, say so to the coordinator, and fix the check that day.

A run that's going is never cancelled by a newer push. The newest push takes the place of one that's
still waiting, and since each commit holds the ones before it, nothing goes unchecked.

## Outside contributions

Pull requests from outside are still welcome. CI doesn't run on pull requests any more, so a
maintainer runs the gate on the branch before merging it.
