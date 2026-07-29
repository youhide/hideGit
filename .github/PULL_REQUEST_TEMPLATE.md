## What this changes

<!-- One or two sentences. If it closes an issue: "Closes #123". -->

## Why

<!-- The problem being solved, if it is not obvious from the above. -->

## What a reviewer should check

<!-- The most useful part of this template. Point at the parts you are least sure about, the
     tradeoff you made, or the case you could not test locally. -->

## Checklist

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] Tests added or updated for the behaviour changed
- [ ] Documentation in `docs/` updated in this PR, if this changes documented behaviour

## If this touches the Git layer

- [ ] Reads go through `gix`, writes through the `git` CLI — or there is a reason in the
      description why not
- [ ] Any new shell-out passes arguments as a vector, sets `GIT_TERMINAL_PROMPT=0`, and parses a
      machine-readable format
- [ ] Git's `stderr` reaches the user on failure rather than being paraphrased

## If this changes an architectural decision

- [ ] There is a superseding [ADR](../docs/adr/README.md), or an issue proposing one

<!-- Reversing an ADR in passing is the one thing that will get a PR sent back regardless of how
     good the code is. The decision may well be wrong — just change it deliberately. -->
