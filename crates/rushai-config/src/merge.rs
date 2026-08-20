use serde_json::Value;

/// Objects merge recursively; everything else is replaced.
pub(crate) fn deep_merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(&key) {
                    Some(slot) => deep_merge(slot, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (slot, over) => *slot = over,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::deep_merge;

    #[test]
    fn nested_objects_merge_scalars_and_arrays_replace() {
        let mut base = json!({
            "theme": "dark",
            "providers": { "anthropic": { "api_key": "a" } },
            "list": [1, 2]
        });
        deep_merge(
            &mut base,
            json!({
                "providers": { "anthropic": { "base_url": "http://x" }, "openai": {} },
                "list": [3]
            }),
        );
        assert_eq!(
            base,
            json!({
                "theme": "dark",
                "providers": {
                    "anthropic": { "api_key": "a", "base_url": "http://x" },
                    "openai": {}
                },
                "list": [3]
            })
        );
    }
}
