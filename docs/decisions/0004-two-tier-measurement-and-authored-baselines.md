---
status: accepted
date: 2026-07-27
---

# 0004 — CI lanes divide by evidence kind

CI lanes divide evidence by kind, not by observed runtime: stable counts and structural assertions may gate pull requests, while clocks and trends belong to soak and do not fail a pull request. This keeps lane ownership stable as machines and workloads change.
