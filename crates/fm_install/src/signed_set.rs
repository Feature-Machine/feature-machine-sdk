// The signed sets of the feed (policies, notices; plan 2026-11-04-game-policies) have one shape:
// {schema, kind, revision, issued, <list>}, signed with the feed key over the exact bytes for
// "version:<revision>" (the module catalog format). This reads and checks that shape once. The
// caller reads each list item alone, so one item of a newer kind does not stop the whole set.
use serde::Deserialize;

/// The fields that every signed set has.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub schema: u32,
    pub kind: String,
    pub revision: u64,
    pub issued: u64,
}

#[derive(Deserialize)]
struct Raw {
    schema: u32,
    kind: String,
    revision: u64,
    issued: u64,
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

/// Checks the signature of `bytes` with `key` for its revision, then the `kind` and `schema` (a
/// file signed for one purpose never passes as another). Gives the header and the raw items of
/// the `list` field. Errors start with the kind name.
pub fn read(bytes: &[u8], signature: &str, key: &str, kind: &str, schema: u32, list: &str) -> Result<(Header, Vec<serde_json::Value>), String> {
    let mut raw: Raw = serde_json::from_slice(bytes).map_err(|e| format!("{kind}: {e}"))?;
    crate::package::verify(bytes, signature, key, &raw.revision.to_string()).map_err(|e| format!("{kind}: {e}"))?;
    if raw.kind != kind || raw.schema != schema {
        return Err(format!("{kind}: unknown kind or schema"));
    }
    let items = match raw.rest.remove(list) {
        Some(serde_json::Value::Array(items)) => items,
        _ => return Err(format!("{kind}: no {list} list")),
    };
    Ok((Header { schema: raw.schema, kind: raw.kind, revision: raw.revision, issued: raw.issued }, items))
}
