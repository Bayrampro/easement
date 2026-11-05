---
status: proposed
---

# A narrowed verdict and an evidence gap are reported separately

A verdict can fall short of the whole project for two unrelated reasons. One is that the caller asked for a part (`--diff`, `--component`, `--scope`): a narrowed verdict. The other is that the check could not read or judge some code: an evidence gap. One `confidence: partial` value plus a flat list of prose reasons hid which of the two applied, so an agent could neither gate on missing evidence without rejecting every deliberate diff run, nor see accepted gaps (`uncovered_ok`), which were dropped from the output. So reports carry narrowing, evidence gaps and accepted gaps as separate typed lists. Cache provenance is a fourth, independent note: a cache hit is not a gap. `confidence` and `confidence_reasons` remain for compatibility and are derived from the same lists.

## Considered options

- **Keep one `confidence` value and add a gate flag that filters its reasons.** Rejected because it treats the symptom: the gate would have to re-parse prose, and every other consumer would still be unable to tell the two cases apart.
- **Drop `confidence` in favour of the new lists.** Deferred: agents already read it, so it stays until a deliberate breaking release.
