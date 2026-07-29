---
status: accepted
date: 2026-07-28
---

# 0006 — recorded corpus inputs are self-contained

Each recorded corpus input is a self-contained exact tree, with every file's bytes carried in its manifest. Profile and seed are labels on the recording rather than regeneration instructions, so corpus activation never depends on the fixture generator.
