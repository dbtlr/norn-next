---
status: accepted
date: 2026-07-28
---

# 0008 — dependency equality is restricted to present crates

The architecture gate requires exact equality between observed workspace normal-dependency edges and the allowlist restricted to crates currently present. This keeps the target crate map ahead of implementation without requiring edges to unearned crates, while refusing both undeclared edges and unused permissions among earned crates.
