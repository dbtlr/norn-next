# Check the NORN-422 links

This temporary folder tests link resolution inside your existing vault.
All results start unobserved. A broken link is a valid observation.

## Record the environment

1. Wait for the entire folder to sync before testing.
2. Open [the results sheet](n422-expectations.md).
3. Fill in the environment fields.
4. Check that the probe folder is `artifacts/scratch/norn-422` relative to your vault root.
5. Search the vault for `n422-` and record any matching documents outside this folder.
6. Record your original link settings before changing them for the creation tests.

Sync may change Unicode filename spellings. Record that uncertainty if you cannot inspect filename code points.
Case mismatch observations on a case-insensitive filesystem do not settle behavior on a case-sensitive filesystem.
Do not rename or remove existing documents outside this folder.

## Open the numbered links

1. Open [the main probes](sources/n422-links.md).
2. Open each numbered link in order.
3. Record the destination's complete vault-relative path and visible target marker in the results sheet.
4. For heading and block links, record whether Obsidian reaches the requested marker.
5. If a link offers to create a note, record `unresolved` without creating it.
6. Open [the remote ambiguity probes](observer/n422-remote-links.md).
7. Record both remote results.

Use reading view or your normal link-opening gesture.
An attachment opens an image rather than a Markdown target marker.
The `photo.png` target is a one-pixel image; `photo.md` displays `PHOTO-MARKDOWN`.
Missing headings and blocks test the anchor independently from the destination document.

## Observe rename behavior

Complete the opening checks before renaming anything.
For each case, use the target you actually observed, rather than guessing from the link spelling.

1. Set **Automatically update internal links** on and record that setting.
2. Copy the case's source line inside a fenced rename entry in the results sheet.
3. If the observed target is inside this folder, rename it in Obsidian's file explorer.
4. Add `-renamed` before the target's final extension, preserving its folder.
5. Wait for Obsidian to finish updating links. Check that original examples and prior observation records remain unchanged.
6. Copy the resulting source line verbatim inside that fenced entry, even if unchanged.
7. Open the resulting link and record its destination and anchor.
8. Undo the rename through Obsidian before testing the next case.
9. Compare the source line with [the original lines](n422-originals.md) before the next rename.

One rename may rewrite several numbered links. Capture each affected line under its own case ID.
If undo does not restore the source bytes, stop that rename sequence and request a fresh working copy.
If Obsidian changes original examples or prior records, stop and preserve the evidence outside Obsidian before continuing.
Preserve observations before resetting a working copy. Never copy over the only results sheet.
For unresolved cases, record `not applicable` with the reason instead of creating a target.
If a link opens a document outside this folder, record the result and skip its rename.
An outside destination's rename remains unverified until reproduced in a controlled copy.

## Record links Obsidian creates

Use the six fenced records under **Links Obsidian creates** in the results sheet.
Test every combination of **New link format** and **Use wikilinks** shown there.

1. Create `sources/n422-authoring.md` inside this folder.
2. Choose the record's two link settings.
3. Use Obsidian's link completion to insert a link to `targets/n422-space name.md`.
4. Copy the resulting source bytes into the fenced record.
5. Open the link and record the destination's complete path.
6. Rename that target to `n422-space name-renamed.md` through Obsidian.
7. Copy the updated source bytes and record the destination after opening the link again.
8. Undo the rename and restore the source before testing the next combination.
9. Restore your original link settings when all rows are complete.

## Hand back the observations

Save the results sheet and allow it to sync.
Keep unknown results marked uncertain. Do not fill them from documentation or a predicted rule.
The observations inform a later resolution decision; they do not change Norn's rules themselves.
Keep this folder until the results and any supporting source changes have been collected.
