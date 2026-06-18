---
name: rebase-rksm-master
description: Rebase the rksm/herdr fork branch onto the upstream Herdr master branch and push it back to rksm. Use when asked to sync, rebase, or update rksm/master, rksm/main, or the rksm fork against origin/master or upstream Herdr.
---

# Rebase rksm/master

Use this skill only inside the Herdr repository.

Herdr currently uses `master` as the branch name. If the user says `main`, verify refs with `git ls-remote --heads origin main master` and `git ls-remote --heads rksm main master`, then use the branch that actually exists. Do not create or rename branches as part of this workflow.

## Workflow

1. Check repository state and remotes.

   ```bash
   git status --short --branch
   git remote -v
   git branch --show-current
   ```

   If the shared checkout has unrelated changes, do not touch them. Prefer a temporary sibling worktree for the rebase.

2. Fetch refs and record the pre-rebase state.

   ```bash
   git fetch origin
   git fetch rksm
   base="$(git merge-base origin/master rksm/master)"
   old_rksm_head="$(git rev-parse rksm/master)"
   origin_head="$(git rev-parse origin/master)"
   git log "$base..origin/master" --oneline
   git log --oneline --left-right --cherry-pick origin/master...rksm/master
   ```

   Preserve the `git log "$base..origin/master" --oneline` output for the final report as the upstream changes now included in `rksm/master`.

3. Rebase in an isolated worktree when useful.

   ```bash
   mkdir -p ../herdr-worktrees
   git worktree add --detach ../herdr-worktrees/rebase-rksm-master rksm/master
   cd ../herdr-worktrees/rebase-rksm-master
   git rebase origin/master
   ```

   If the main checkout is clean and already on the target branch, rebasing there is acceptable. Keep the recorded `base`, `old_rksm_head`, and `origin_head` values from before the rebase.

4. Resolve conflicts by preserving both intentions.

   For each conflict, inspect both sides before editing:

   ```bash
   git status --short
   rg -n "<<<<<<<|=======|>>>>>>>" <conflicted-files>
   git show --stat --oneline REBASE_HEAD
   git show REBASE_HEAD -- <conflicted-files>
   git log --oneline --reverse "$base..origin/master" -- <conflicted-files>
   git show <upstream-commit> -- <conflicted-files>
   ```

   Treat `REBASE_HEAD` as the rksm commit being replayed. Treat commits in `$base..origin/master` touching the same files as the upstream intention. Resolve only after understanding what each side was trying to preserve.

   Maintain both intentions whenever they are compatible. Use current upstream structure and APIs as the base, then apply the rksm behavior on top.

   If upstream already implements the exact same feature as the rksm commit, report that clearly and drop the rksm commit:

   ```bash
   git rebase --skip
   ```

   If there is a conceptual conflict and no honest resolution preserves both intentions, stop. Do not push. Report the paused rebase state, conflict files, the upstream intention, the rksm intention, and why the conflict needs a human decision.

   After resolving a conflict:

   ```bash
   git diff --check
   git add <resolved-files>
   GIT_EDITOR=true git rebase --continue
   ```

   Repeat until the rebase completes.

5. Validate before pushing.

   If conflict resolution changed code, run the narrowest relevant `just` command in the Nix dev shell. Prefer a targeted test filter when the conflict touched a focused behavior.

   ```bash
   nix develop -c just test-one <filter>
   ```

   If the dev shell is missing a required tool, add it through Nix rather than installing globally. For example:

   ```bash
   nix develop -c nix shell nixpkgs#clang -c just test-one <filter>
   ```

   If validation cannot run, report the exact command and failure.

6. Push to `rksm` with a lease.

   Verify `rksm/master` has not moved since `old_rksm_head`:

   ```bash
   git ls-remote --heads rksm master
   git push --force-with-lease=master:"$old_rksm_head" rksm HEAD:master
   ```

   After pushing, refresh and verify:

   ```bash
   git fetch rksm master
   git rev-parse rksm/master
   git ls-remote --heads rksm master
   ```

7. Clean up temporary worktrees.

   ```bash
   git worktree remove --force ../herdr-worktrees/rebase-rksm-master
   git worktree list
   ```

## Final Report

Report these items concisely:

- Branch mapping, especially if the user said `main` but the repo uses `master`.
- Old and new `rksm/master` commits.
- `origin/master` commit used as the rebase base.
- Conflicts encountered, with files, conflicting rksm commit, relevant upstream commit or commits, and the resolution.
- Any rksm commits skipped because upstream already implemented the same feature.
- Validation command and result.
- Upstream changes now included in `rksm/master`, based on the saved `git log "$base..origin/master" --oneline` output.

Do not hide unrelated dirty worktree files. Mention that they were left untouched when present.
