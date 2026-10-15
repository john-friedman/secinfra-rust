# XBRL

`parse_xbrl` reads SEC inline HTML or extracted-inline XML into full parsed facts.
`construct_simple_xbrl` projects those facts into flat records for the Simple XBRL
table. Neither function performs network or storage operations.

```rust,no_run
use secinfra::{XbrlFileType, construct_simple_xbrl, parse_xbrl};

let bytes = std::fs::read("filing_htm.xml")?;
let facts = parse_xbrl(&bytes, XbrlFileType::ExtractedInline)?;
let full_json = serde_json::to_vec(&facts)?;
let simple_records = construct_simple_xbrl(32019326000001, &facts);
# Ok::<(), anyhow::Error>(())
```

Use `XbrlFileType::Inline` for the HTML filing. Inline parsing accepts HTML void
tags, entities and lowercase attribute names. XML parsing reports malformed input.

## Full facts

Each `XbrlFact` serializes as `_val`, `_attributes`, `_context` and, when resolved,
`_unit`. Empty values and unresolved contexts or units are omitted. `_attributes`
remains an `XbrlMap` of strings.
As in [secxbrl](https://github.com/john-friedman/secxbrl), `contextRef` is resolved
into `_context` and removed from the attributes.

`_context` is an `XbrlContext`: a map of context leaf paths to
`XbrlContextValue::String`, `XbrlContextValue::List` or
`XbrlContextValue::Dimensions`. The enum serializes directly to JSON strings and
arrays, without enum tags. `_dimensions` contains axis/value pairs with a `type`
of `explicit` or `typed`, preserving their order in the original context.

```json
{
  "_val": "123000",
  "_attributes": {"name": "example:Revenue", "unitRef": "usd"},
  "_context": {
    "_contextref": "duration",
    "entity_identifier": "0000320193",
    "period_startdate": "2025-01-01",
    "period_enddate": "2025-12-31",
    "entity_segment_explicitmember": ["example:ProductMember", "example:USMember"],
    "_dimensions": [
      {"dimension": "example:ProductAxis", "type": "explicit", "value": "example:ProductMember"},
      {"dimension": "example:RegionAxis", "type": "explicit", "value": "example:USMember"},
      {"dimension": "example:CustomerAxis", "type": "typed", "value": "00123"}
    ]
  },
  "_unit": {"numerator": ["USD"], "denominator": []}
}
```

Context paths exclude the context root, use lowercase local element names and
join descendants with underscores. Traversal includes custom descendants such as
typed-member leaves and scenario values. Explicit-member leaves are always arrays,
even for a single member, and retain document order. Both input formats use this
shape. This also corrects secxbrl's inline `context_` prefix inconsistency and its
resulting overwrite of repeated explicit members. Explicit members in scenarios
are retained as arrays too.

This is the parsed fact representation, not the original XML hierarchy. As in
secxbrl, other repeated leaves at the same path use the last value, and context
element attributes other than dimension axes are not included. Typed dimension
values use trimmed descendant text, retaining identifiers as strings. Their XML
markup and domain-element names are not part of the dimension entry.

`_unit` holds the measures from the referenced unit definition as numerator and
denominator lists. Currency and XBRL instance measures use names such as `USD`,
`shares` and `pure`. Other namespace-qualified measures use `{namespace-uri}name`,
so custom measures do not depend on a filing's namespace prefixes. Unit references
without a matching definition remain unresolved. Original documents remain the
source for schemas and other XML outside parsed facts, units and context values.

## Simple XBRL

`construct_simple_xbrl(accession_number, &facts)` leaves the full facts unchanged
and returns `SimpleXbrlRecord`: `accessionNumber`, `context_id`,
`taxonomy`, `name`, `value`, `period_start_date`, `period_end_date`, and `members`.
It applies numeric scaling and joins member arrays with commas only in the flat
records. Context IDs are assigned in first-seen order within a filing.

The records also include:

- `dimensions`: the context's explicit and typed axis/value pairs. Contexts without
  dimensions use an empty list. Existing `members` remains available for callers
  using the older comma-separated representation of explicit members.
- `unit`: a resolved expression such as `USD`, `shares`, `pure` or `USD/shares`.
  Multiple numerator or denominator measures are joined with `*`; a denominator
  containing multiple measures is enclosed in parentheses. Non-numeric facts and
  unresolved units use `null`.
- `decimals` and `precision`: the source accuracy attributes as optional strings,
  including `INF`. They are not used to rescale the value.

Existing JSON facts with string-only contexts can still be deserialized. The
conversion also accepts older Rust `dimension:<axis>` member entries, preserving
their axes in `dimensions`. Older facts without resolved units or dimension axes
cannot recover those details; reparse the original document to obtain them.
Rust callers
that constructed `XbrlFact` directly must use `XbrlContext` for `context`; use
`XbrlContextValue::as_str()`, `as_list()` or `as_dimensions()` to read context values.
`XbrlFact` now also has an optional `unit` field. Existing JSON facts and Simple
XBRL records without the added fields remain deserializable.
