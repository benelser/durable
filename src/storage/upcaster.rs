//! Event schema upcaster — transforms old event versions to current format.
//!
//! When the event schema evolves (new fields, renamed fields, structural changes),
//! register an upcaster that transforms events from version N to N+1.
//! During deserialization, the chain is applied iteratively until the event
//! reaches `CURRENT_SCHEMA_VERSION`.
//!
//! ```rust,ignore
//! let mut registry = UpcasterRegistry::new();
//! registry.register("step_started", 1, |val| {
//!     // Transform v1 step_started to v2 format
//!     // e.g., add a new "occurrence" field with default 0
//!     let mut obj = val;
//!     if let Some(map) = obj.as_object_mut() {
//!         map.insert("occurrence".to_string(), json::json_num(0.0));
//!     }
//!     obj
//! });
//! ```

use crate::json::Value;
use std::collections::BTreeMap;

type UpcasterFn = Box<dyn Fn(Value) -> Value + Send + Sync>;

/// Registry of event schema upcasters.
///
/// Each upcaster transforms an event's JSON from version N to version N+1.
/// Multiple upcasters can be chained for multi-version upgrades.
pub struct UpcasterRegistry {
    /// Map of (event_type_name, from_version) -> transform function
    upcasters: BTreeMap<(String, u32), UpcasterFn>,
}

impl UpcasterRegistry {
    pub fn new() -> Self {
        Self {
            upcasters: BTreeMap::new(),
        }
    }

    /// Register an upcaster that transforms events of the given type
    /// from `from_version` to `from_version + 1`.
    pub fn register<F>(
        &mut self,
        event_type_name: &str,
        from_version: u32,
        transform: F,
    ) where
        F: Fn(Value) -> Value + Send + Sync + 'static,
    {
        self.upcasters.insert(
            (event_type_name.to_string(), from_version),
            Box::new(transform),
        );
    }

    /// Apply upcasters to transform an event's JSON from its current version
    /// to the target version. Returns the transformed JSON and final version.
    pub fn upcast(
        &self,
        event_type_name: &str,
        mut event_json: Value,
        from_version: u32,
        to_version: u32,
    ) -> (Value, u32) {
        let mut current = from_version;
        while current < to_version {
            let key = (event_type_name.to_string(), current);
            if let Some(transform) = self.upcasters.get(&key) {
                event_json = transform(event_json);
                current += 1;
            } else {
                // No upcaster for this version — stop
                break;
            }
        }
        (event_json, current)
    }

    /// Whether any upcasters are registered.
    pub fn is_empty(&self) -> bool {
        self.upcasters.is_empty()
    }
}

impl Default for UpcasterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    #[test]
    fn test_upcaster_chain() {
        let mut registry = UpcasterRegistry::new();

        // v1 -> v2: add "occurrence" field
        registry.register("step_started", 1, |mut val| {
            if let Some(map) = val.as_object_mut() {
                map.insert("occurrence".to_string(), json::json_num(0.0));
            }
            val
        });

        // v2 -> v3: rename "params" to "parameters"
        registry.register("step_started", 2, |mut val| {
            if let Some(map) = val.as_object_mut() {
                if let Some(params) = map.remove("params") {
                    map.insert("parameters".to_string(), params);
                }
            }
            val
        });

        let v1_event = json::json_object(vec![
            ("type", json::json_str("step_started")),
            ("step_name", json::json_str("my_step")),
            ("params", json::json_str("{}")),
        ]);

        // Upcast from v1 to v3
        let (result, final_version) = registry.upcast("step_started", v1_event, 1, 3);
        assert_eq!(final_version, 3);
        assert!(result.get("occurrence").is_some(), "v2 added occurrence");
        assert!(result.get("parameters").is_some(), "v3 renamed to parameters");
        assert!(result.get("params").is_none(), "v3 removed params");
    }

    #[test]
    fn test_upcaster_no_op_when_current() {
        let registry = UpcasterRegistry::new();
        let val = json::json_object(vec![("type", json::json_str("test"))]);
        let (result, version) = registry.upcast("test", val.clone(), 1, 1);
        assert_eq!(version, 1);
        assert_eq!(result, val);
    }

    #[test]
    fn test_upcaster_stops_at_gap() {
        let mut registry = UpcasterRegistry::new();
        registry.register("test", 1, |val| val); // only v1->v2

        let val = json::json_object(vec![]);
        let (_result, version) = registry.upcast("test", val, 1, 5);
        assert_eq!(version, 2, "stops when no upcaster for v2->v3");
    }
}
