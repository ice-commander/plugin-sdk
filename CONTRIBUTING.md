# Contributing

Patches are welcome. Two things are required.

## Sign your commits

Every commit must carry a `Signed-off-by` line, which certifies the Developer
Certificate of Origin in [DCO](DCO):

```sh
git commit -s
```

To never forget it, install the hook that adds the line to every commit message:

```sh
cat > .git/hooks/prepare-commit-msg <<'HOOK'
#!/bin/sh
git interpret-trailers --in-place --if-exists doNothing --trailer "Signed-off-by: $(git config user.name) <$(git config user.email)>" "$1"
HOOK
chmod +x .git/hooks/prepare-commit-msg
```

The name and e-mail in the line must match the commit author. `git commit
--amend -s` fixes the last commit; `git rebase --signoff <base>` fixes a branch.

## Licence

This repository is dual licensed under [Apache 2.0](LICENSE-APACHE) and
[MIT](LICENSE-MIT), at your option. A contribution is accepted under the same
terms, with no additional conditions.
