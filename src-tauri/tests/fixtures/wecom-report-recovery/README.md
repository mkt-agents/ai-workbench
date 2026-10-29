# Report recovery replay fixtures

`seq-47.json`, `seq-55.json`, and `seq-63.json` preserve the assistant text report shapes from a captured failed investigation: bare report, envelope without `queries`, then full envelope. Only the three `assistant/message` text blocks were projected; no reasoning blocks are included.

Business IDs, company ID, products and package prefix are replaced with fixture identifiers. Code paths, line numbers and excerpts target the original miniature `rule.rs`, not the investigated source tree. `evidence.txt` is synthetic producer-shaped evidence reproducing the reference graph and decision observations; it is not an assertion that the original raw records were captured here.

The reports retain a string subject, eight timeline entries referencing ten record IDs, and four code checks whose explicit log references intersect the trigger's L5/L6/L8 references. Recovery may bind these declared references; it must not invent causal facts, change report text/level, discard coverage gaps, or accept nonexistent records/source lines.
