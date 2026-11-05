---
status: proposed
---

# Diff mode attributes the policy delta by default

`--diff` promises to judge a pull request. A change to the policy is part of the pull request, so a violation also belongs to the change when it is in the policy delta: it is produced under the head policy and not under the base policy, over the same current graph. This is the default, not a flag, because an opt-in would leave the hole open for everyone who does not know about it. The price is a full judgment, with the reason stated, when the two policies differ in what gets scanned or a manifest changed. In that case, teams that rely only on `--diff` will see old violations that the baseline does not cover.

## Considered options

- **A separate flag or command for policy changes.** Rejected: it treats the symptom, which is the README's advice to "run a full check for rule changes", by hand.
- **Always judge in full when a policy input changed.** Rejected: unrelated old debt would surface on every policy edit, although the delta can say exactly what the edit caused.
