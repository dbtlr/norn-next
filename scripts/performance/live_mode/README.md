# Public first-open validation helpers

`public_mode.py` validates the public first-open events recorded by
`record.py`. The content, provenance, and producer validators remain available
for those records and their tests.

The former comparison controller depended on retired external intake runners
and has been removed. Run first-answer measurements through
`baseline.py --loading operator-prefix-public` instead.
