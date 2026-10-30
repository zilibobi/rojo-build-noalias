use std::collections::BTreeMap;

/// The `aliases` map of a `.luaurc`, parsed as strict JSON (no comments, no trailing commas).
/// A missing or null `aliases` is an empty map.
pub fn parse_aliases(text: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let value: serde_json::Value = serde_json::from_str(text)?;

    let Some(obj) = value.as_object() else {
        anyhow::bail!("expected a JSON object at the top level");
    };

    match obj.get("aliases") {
        None | Some(serde_json::Value::Null) => Ok(BTreeMap::new()),
        Some(aliases) => Ok(serde_json::from_value(aliases.clone())?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_map_parses() {
        let map = parse_aliases(
            r#"{"languageMode": "strict", "aliases": {"vfx": "./src/vfx", "lib": "./lib"}}"#,
        )
        .unwrap();
        assert_eq!(
            map,
            BTreeMap::from([
                ("lib".to_string(), "./lib".to_string()),
                ("vfx".to_string(), "./src/vfx".to_string()),
            ])
        );
    }

    #[test]
    fn missing_or_null_aliases_is_empty() {
        assert!(parse_aliases("{}").unwrap().is_empty());
        assert!(parse_aliases(r#"{"aliases": null}"#).unwrap().is_empty());
    }

    #[test]
    fn strict_json_only() {
        assert!(parse_aliases(r#"{"aliases": {"a": "./a"},}"#).is_err());
        assert!(parse_aliases("{\n// c\n}").is_err());
        assert!(parse_aliases(r#"{"aliases": {"a": 1}}"#).is_err());
        assert!(parse_aliases("[]").is_err());
    }
}
