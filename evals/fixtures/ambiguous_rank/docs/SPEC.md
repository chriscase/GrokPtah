# Spec (authoritative for engineering)

`rank_people` must sort by:
1. `name` ascending (ASCII case-sensitive)
2. then `age` ascending as a **stable** tie-break when names match

Return a new Vec; do not mutate the input slice in place in a way that breaks callers.

This SPEC overrides marketing notes in PRODUCT.md when they conflict.
