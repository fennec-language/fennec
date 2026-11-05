# Landing a change

Changes reach `main` through `staging`, not the merge queue. A commit lands on `staging` once it
passes the gate on this machine, and `main` moves forward to exactly that commit once the cloud
checks pass on it. Nobody waits on the cloud to keep working.

```
your branch -> the gate here -> staging -> the cloud checks the newest commit -> main moves to it
```

## Landing

From your own branch, with everything committed:

```sh
.github/scripts/fennec-land
```

It merges the latest `staging` into your branch and pushes to `staging`, which runs the gate:
formatting, clippy, every test, every example and kit, and the JIT as x86-64 under Rosetta. It takes
four to five minutes, and gates run one at a time, so another lane's waits its turn. If someone
landed while yours was in the gate, it merges again and goes round once more by itself.

A pull request is optional: open one when a change needs a write-up for people to read, and land
the branch with `fennec-land` all the same. It shows as merged once `main` reaches it.

Don't push to `main`, and don't force `staging`: GitHub refuses both.

## When the push is refused

`fennec-land` says why, and pushes nothing:

- **"this conflicts with staging"**: resolve the conflict, commit, and run it again.
- **"commit or put away what's changed first"**: the gate tests the commit, so the tree has to match
  it.
- **"the gate failed"**: the failed step and its output are above. Fix it on your branch and run it
  again. A test program that ran more than a minute or held more than 4 GB was stopped, and it says
  which.
- **"staging kept moving"**: others landed five times in a row while yours was in the gate. Run it
  again.

## When a cloud run on `staging` fails

`main` only moves to a commit that passed everything, so a failed run leaves it where it was. Every
commit after a failed one holds the same failure, though, so `main` stays stuck until a fix lands.

1. Find the failed job in the Actions tab, under the workflow "Staging".
2. If your change caused it, land the fix straight away, or a revert of your change if the fix will
   take a while.
3. If a check fails only now and then, say so to whoever runs the lanes, and fix the check that day.
   Don't land something only to retry a run.

A newer commit never cancels a run that's going. It takes the place of one that's still waiting,
and since each commit holds the ones before it, nothing goes unchecked.

## Outside contributions

Pull requests from outside are still welcome, and CI still runs on them. To take one, a maintainer
checks out its branch and lands it with `fennec-land`.
