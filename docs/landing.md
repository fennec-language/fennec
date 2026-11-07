# Landing a change

A change merges into `main` as soon as it's ready, without waiting for CI. The checks that matter
most run first on your own machine, and CI runs everything on `main` after the merge. A red run on
`main` gets a fix or a revert straight away.

```
your branch -> the gate on your machine -> a pull request to main -> merged -> CI on main
```

## Landing

With everything committed on your branch, write what the pull request says in a file, and run:

```sh
.github/scripts/open-pr.sh "What it does, in a line" body.md
```

It does the three steps below in order, and stops at the first that fails, with nothing pushed:

1. It merges the latest `main` into your branch, so that the gate tests what will be on `main`. A
   merge that conflicts is undone, so your branch is as it was: merge `main` yourself, resolve it,
   commit, and run it again.
2. It runs the gate, `.github/scripts/gate.sh`. That checks formatting, runs clippy, every test and
   every example and kit, and runs the JIT as x86-64 under Rosetta. It stops at the first failure and
   prints that step's output. A test program that runs more than a minute or holds more than 4 GB is
   stopped, and the gate says which. It takes four to five minutes when the build is warm.
3. It pushes your branch, checks that origin has this very commit, and opens the pull request to
   `main`. The coordinator merges it.

Say in the text that the gate passed, and what you measured. Don't open a pull request by joining these
steps by hand in one command: a failed merge or gate goes unnoticed that way, and has more than once.

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
