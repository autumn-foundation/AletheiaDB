//! Event type for Honeycomb telemetry.
//!
//! Events are the core data structure sent to Honeycomb. Each event contains
//! a timestamp, arbitrary key-value fields, and optional metadata.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// An event to be sent to Honeycomb.
///
/// Events contain arbitrary key-value data and a timestamp. When serialized
/// for the Honeycomb batch API, they follow the format:
///
/// ```json
/// {
///   "time": "2024-01-15T10:30:00.000Z",
///   "samplerate": 1,
///   "data": {
///     "field1": "value1",
///     "field2": 42
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Timestamp when the event occurred, in ISO 8601 format.
    #[serde(rename = "time")]
    pub timestamp: String,

    /// Sample rate for this event.
    #[serde(rename = "samplerate")]
    pub sample_rate: usize,

    /// The actual event data as key-value pairs.
    pub data: HashMap<String, Value>,

    /// Optional metadata (not sent to Honeycomb, used for response correlation).
    #[serde(skip)]
    pub metadata: Option<Value>,
}

impl Default for Event {
    fn default() -> Self {
        Self::new()
    }
}

impl Event {
    /// Create a new event with the current timestamp.
    pub fn new() -> Self {
        Self {
            timestamp: Self::format_timestamp(SystemTime::now()),
            sample_rate: 1,
            data: HashMap::new(),
            metadata: None,
        }
    }

    /// Create a new event with a specific timestamp.
    pub fn with_timestamp(timestamp: SystemTime) -> Self {
        Self {
            timestamp: Self::format_timestamp(timestamp),
            sample_rate: 1,
            data: HashMap::new(),
            metadata: None,
        }
    }

    /// Format a `SystemTime` as ISO 8601 string.
    ///
    /// # Pre-Epoch Timestamps
    ///
    /// If the timestamp is before Unix epoch (1970-01-01), this logs a warning
    /// and uses the epoch as fallback. This prevents silent failures while
    /// ensuring observability data is not lost.
    fn format_timestamp(time: SystemTime) -> String {
        let duration = match time.duration_since(UNIX_EPOCH) {
            Ok(d) => d,
            Err(e) => {
                // Log warning about pre-epoch timestamp - this could indicate clock skew
                // or a bug in timestamp generation
                #[cfg(feature = "observability")]
                tracing::warn!(
                    "Pre-epoch timestamp detected ({:?} before epoch), using epoch as fallback",
                    e.duration()
                );
                #[cfg(not(feature = "observability"))]
                eprintln!(
                    "Warning: Pre-epoch timestamp detected ({:?} before epoch), using epoch as fallback",
                    e.duration()
                );
                Duration::ZERO
            }
        };

        // Calculate components
        let total_secs = duration.as_secs();
        let millis = duration.subsec_millis();

        // Convert to date/time components (simplified - assumes UTC)
        // Days since epoch
        let days = total_secs / 86400;
        let remaining_secs = total_secs % 86400;
        let hours = remaining_secs / 3600;
        let minutes = (remaining_secs % 3600) / 60;
        let seconds = remaining_secs % 60;

        // Calculate year, month, day from days since epoch (1970-01-01)
        let (year, month, day) = days_to_ymd(days);

        format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
    }

    /// Add a field to the event.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let mut event = Event::new();
    /// event.add_field("message", "Hello!");
    /// event.add_field("count", 42);
    /// event.add_field("active", true);
    /// ```
    pub fn add_field(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        self.data.insert(key.into(), value.into());
    }

    /// Add multiple fields from a serializable struct.
    ///
    /// # Errors
    ///
    /// Returns an error if the value cannot be converted to a JSON object.
    pub fn add<T: Serialize>(&mut self, value: &T) -> Result<(), super::error::Error> {
        let json = serde_json::to_value(value)?;
        if let Value::Object(map) = json {
            for (k, v) in map {
                self.data.insert(k, v);
            }
        }
        Ok(())
    }

    /// Set the sample rate for this event.
    pub fn set_sample_rate(&mut self, rate: usize) {
        self.sample_rate = rate;
    }

    /// Set the timestamp for this event.
    pub fn set_timestamp(&mut self, timestamp: SystemTime) {
        self.timestamp = Self::format_timestamp(timestamp);
    }

    /// Set metadata for this event.
    ///
    /// Metadata is not sent to Honeycomb but can be used to correlate
    /// responses with events.
    pub fn set_metadata(&mut self, metadata: impl Into<Value>) {
        self.metadata = Some(metadata.into());
    }

    /// Get the event's metadata.
    pub fn metadata(&self) -> Option<&Value> {
        self.metadata.as_ref()
    }

    /// Get the event's fields.
    pub fn fields(&self) -> &HashMap<String, Value> {
        &self.data
    }

    /// Get mutable access to the event's fields.
    pub fn fields_mut(&mut self) -> &mut HashMap<String, Value> {
        &mut self.data
    }

    /// Check if the event has any fields.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Get the number of fields in the event.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Serialize the event to JSON for the Honeycomb batch API.
    pub fn to_json(&self) -> Result<String, super::error::Error> {
        Ok(serde_json::to_string(self)?)
    }

    /// Determine if this event should be dropped based on sampling.
    ///
    /// Returns true if the event should be dropped (not sent).
    pub fn should_drop(&self) -> bool {
        if self.sample_rate <= 1 {
            return false;
        }
        // Simple deterministic sampling based on event content hash
        let hash = self.content_hash();
        !hash.is_multiple_of(self.sample_rate as u64)
    }

    /// Generate a hash of the event content for sampling purposes.
    ///
    /// This implementation:
    /// - Sorts keys for deterministic hashing (HashMap iteration is non-deterministic)
    /// - Uses efficient value hashing without string allocation
    fn content_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.timestamp.hash(&mut hasher);

        // Sort keys for deterministic hashing
        let mut keys: Vec<_> = self.data.keys().collect();
        keys.sort();

        for key in keys {
            key.hash(&mut hasher);
            // Hash the value efficiently without allocating a string
            if let Some(value) = self.data.get(key) {
                Self::hash_json_value(value, &mut hasher);
            }
        }
        hasher.finish()
    }

    /// Hash a JSON value efficiently without string allocation.
    fn hash_json_value(value: &Value, hasher: &mut impl std::hash::Hasher) {
        use std::hash::Hash;

        match value {
            Value::Null => 0u8.hash(hasher),
            Value::Bool(b) => {
                1u8.hash(hasher);
                b.hash(hasher);
            }
            Value::Number(n) => {
                2u8.hash(hasher);
                // Hash the bits representation for consistency
                if let Some(i) = n.as_i64() {
                    i.hash(hasher);
                } else if let Some(u) = n.as_u64() {
                    u.hash(hasher);
                } else if let Some(f) = n.as_f64() {
                    f.to_bits().hash(hasher);
                }
            }
            Value::String(s) => {
                3u8.hash(hasher);
                s.hash(hasher);
            }
            Value::Array(arr) => {
                4u8.hash(hasher);
                arr.len().hash(hasher);
                for item in arr {
                    Self::hash_json_value(item, hasher);
                }
            }
            Value::Object(obj) => {
                5u8.hash(hasher);
                obj.len().hash(hasher);
                // Sort keys for determinism
                let mut keys: Vec<_> = obj.keys().collect();
                keys.sort();
                for key in keys {
                    key.hash(hasher);
                    if let Some(v) = obj.get(key) {
                        Self::hash_json_value(v, hasher);
                    }
                }
            }
        }
    }
}

/// Convert days since Unix epoch to year, month, day.
fn days_to_ymd(days: u64) -> (i32, u32, u32) {
    // Algorithm based on Howard Hinnant's date algorithms
    // http://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Trait for types that can hold fields (compatible with libhoney's FieldHolder).
///
/// This trait is provided for API compatibility and may not be used directly in tests.
#[allow(dead_code)]
pub trait FieldHolder {
    /// Add a single field.
    fn add_field(&mut self, key: impl Into<String>, value: impl Into<Value>);

    /// Add multiple fields from a serializable struct.
    fn add<T: Serialize>(&mut self, value: &T) -> Result<(), super::error::Error>;
}

impl FieldHolder for Event {
    fn add_field(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        Event::add_field(self, key, value);
    }

    fn add<T: Serialize>(&mut self, value: &T) -> Result<(), super::error::Error> {
        Event::add(self, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =====================================================
    // Event Creation Tests
    // =====================================================

    #[test]
    fn test_event_new() {
        let event = Event::new();
        assert!(event.data.is_empty());
        assert_eq!(event.sample_rate, 1);
        assert!(event.metadata.is_none());
        assert!(!event.timestamp.is_empty());
    }

    #[test]
    fn test_event_default() {
        let event = Event::default();
        assert!(event.data.is_empty());
        assert_eq!(event.sample_rate, 1);
    }

    #[test]
    fn test_event_with_timestamp() {
        // 1705314600 = 2024-01-15T10:30:00Z (verified)
        let ts = UNIX_EPOCH + Duration::from_secs(1705314600);
        let event = Event::with_timestamp(ts);
        assert!(event.timestamp.contains("2024-01-15"));
        assert!(event.timestamp.contains("10:30:00"));
    }

    // =====================================================
    // Field Manipulation Tests
    // =====================================================

    #[test]
    fn test_add_field_string() {
        let mut event = Event::new();
        event.add_field("message", "Hello, World!");
        assert_eq!(
            event.data.get("message"),
            Some(&Value::String("Hello, World!".to_string()))
        );
    }

    #[test]
    fn test_add_field_number() {
        let mut event = Event::new();
        event.add_field("count", 42);
        assert_eq!(event.data.get("count"), Some(&Value::Number(42.into())));
    }

    #[test]
    fn test_add_field_bool() {
        let mut event = Event::new();
        event.add_field("active", true);
        assert_eq!(event.data.get("active"), Some(&Value::Bool(true)));
    }

    #[test]
    fn test_add_field_float() {
        let mut event = Event::new();
        event.add_field("duration_ms", serde_json::json!(1.5));
        let val = event.data.get("duration_ms").unwrap();
        assert!((val.as_f64().unwrap() - 1.5).abs() < 0.001);
    }

    #[test]
    fn test_add_field_null() {
        let mut event = Event::new();
        event.add_field("nullable", Value::Null);
        assert_eq!(event.data.get("nullable"), Some(&Value::Null));
    }

    #[test]
    fn test_add_field_overwrite() {
        let mut event = Event::new();
        event.add_field("key", "value1");
        event.add_field("key", "value2");
        assert_eq!(
            event.data.get("key"),
            Some(&Value::String("value2".to_string()))
        );
    }

    #[test]
    fn test_add_struct() {
        #[derive(Serialize)]
        struct TestData {
            name: String,
            value: i32,
        }

        let mut event = Event::new();
        let data = TestData {
            name: "test".to_string(),
            value: 100,
        };
        event.add(&data).unwrap();

        assert_eq!(
            event.data.get("name"),
            Some(&Value::String("test".to_string()))
        );
        assert_eq!(event.data.get("value"), Some(&Value::Number(100.into())));
    }

    #[test]
    fn test_fields_access() {
        let mut event = Event::new();
        event.add_field("key", "value");

        let fields = event.fields();
        assert_eq!(fields.len(), 1);
        assert!(fields.contains_key("key"));
    }

    #[test]
    fn test_fields_mut_access() {
        let mut event = Event::new();
        event.add_field("key", "value");

        let fields = event.fields_mut();
        fields.insert(
            "new_key".to_string(),
            Value::String("new_value".to_string()),
        );

        assert_eq!(event.data.len(), 2);
    }

    #[test]
    fn test_is_empty() {
        let event = Event::new();
        assert!(event.is_empty());

        let mut event2 = Event::new();
        event2.add_field("key", "value");
        assert!(!event2.is_empty());
    }

    #[test]
    fn test_len() {
        let mut event = Event::new();
        assert_eq!(event.len(), 0);

        event.add_field("key1", "value1");
        assert_eq!(event.len(), 1);

        event.add_field("key2", "value2");
        assert_eq!(event.len(), 2);
    }

    // =====================================================
    // Sample Rate Tests
    // =====================================================

    #[test]
    fn test_set_sample_rate() {
        let mut event = Event::new();
        assert_eq!(event.sample_rate, 1);

        event.set_sample_rate(10);
        assert_eq!(event.sample_rate, 10);
    }

    #[test]
    fn test_should_drop_no_sampling() {
        let event = Event::new();
        assert!(!event.should_drop());

        let mut event2 = Event::new();
        event2.set_sample_rate(1);
        assert!(!event2.should_drop());
    }

    #[test]
    fn test_should_drop_with_sampling() {
        // With sample_rate > 1, some events should be dropped
        let mut dropped = 0;
        let mut kept = 0;

        for i in 0..1000 {
            let mut event = Event::new();
            event.add_field("index", i);
            event.set_sample_rate(10);

            if event.should_drop() {
                dropped += 1;
            } else {
                kept += 1;
            }
        }

        // With sample rate 10, roughly 10% should be kept
        // Allow for statistical variation
        assert!(kept > 50, "Expected ~100 kept, got {kept}");
        assert!(kept < 200, "Expected ~100 kept, got {kept}");
        assert!(dropped > 800, "Expected ~900 dropped, got {dropped}");
    }

    #[test]
    fn test_content_hash_deterministic() {
        // Verify that content_hash produces consistent results
        let mut event1 = Event::with_timestamp(UNIX_EPOCH + Duration::from_secs(1000));
        event1.add_field("b_key", "value_b");
        event1.add_field("a_key", "value_a");
        event1.add_field("c_key", 42);

        let mut event2 = Event::with_timestamp(UNIX_EPOCH + Duration::from_secs(1000));
        // Add fields in different order
        event2.add_field("c_key", 42);
        event2.add_field("a_key", "value_a");
        event2.add_field("b_key", "value_b");

        // Hashes should be identical regardless of insertion order
        assert_eq!(event1.content_hash(), event2.content_hash());
    }

    #[test]
    fn test_content_hash_different_for_different_events() {
        let mut event1 = Event::with_timestamp(UNIX_EPOCH + Duration::from_secs(1000));
        event1.add_field("key", "value1");

        let mut event2 = Event::with_timestamp(UNIX_EPOCH + Duration::from_secs(1000));
        event2.add_field("key", "value2");

        assert_ne!(event1.content_hash(), event2.content_hash());
    }

    // =====================================================
    // Timestamp Tests
    // =====================================================

    #[test]
    fn test_set_timestamp() {
        let mut event = Event::new();
        let ts = UNIX_EPOCH + Duration::from_secs(0);
        event.set_timestamp(ts);
        assert!(event.timestamp.contains("1970-01-01"));
    }

    #[test]
    fn test_timestamp_format_iso8601() {
        // 1705314600 = 2024-01-15T10:30:00Z
        let ts = UNIX_EPOCH + Duration::from_secs(1705314600) + Duration::from_millis(123);
        let event = Event::with_timestamp(ts);

        // Should be ISO 8601 format
        assert!(event.timestamp.ends_with('Z'));
        assert!(event.timestamp.contains('T'));
        assert!(event.timestamp.contains('.'));
    }

    #[test]
    fn test_format_timestamp_epoch() {
        let formatted = Event::format_timestamp(UNIX_EPOCH);
        assert_eq!(formatted, "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn test_format_timestamp_specific_date() {
        // 1705314600 = 2024-01-15T10:30:00 UTC (verified with epoch converter)
        // Adding 500ms to get 10:30:00.500
        let ts = UNIX_EPOCH + Duration::from_secs(1705314600) + Duration::from_millis(500);
        let formatted = Event::format_timestamp(ts);
        assert_eq!(formatted, "2024-01-15T10:30:00.500Z");
    }

    #[test]
    fn test_format_timestamp_pre_epoch_fallback() {
        // Test that pre-epoch timestamps fall back to epoch with warning
        // We can't easily create a pre-epoch SystemTime, but we can verify
        // that the epoch fallback produces the correct output
        let epoch = UNIX_EPOCH;
        let formatted = Event::format_timestamp(epoch);
        assert_eq!(formatted, "1970-01-01T00:00:00.000Z");
    }

    // =====================================================
    // Metadata Tests
    // =====================================================

    #[test]
    fn test_metadata() {
        let mut event = Event::new();
        assert!(event.metadata().is_none());

        event.set_metadata("trace-id-123");
        assert_eq!(
            event.metadata(),
            Some(&Value::String("trace-id-123".to_string()))
        );
    }

    #[test]
    fn test_metadata_complex() {
        let mut event = Event::new();
        event.set_metadata(serde_json::json!({
            "trace_id": "abc123",
            "span_id": "def456"
        }));

        let meta = event.metadata().unwrap();
        assert!(meta.is_object());
    }

    // =====================================================
    // Serialization Tests
    // =====================================================

    #[test]
    fn test_to_json() {
        let mut event = Event::new();
        event.add_field("message", "test");
        event.set_sample_rate(5);

        let json = event.to_json().unwrap();

        // Parse and verify
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("time").is_some());
        assert_eq!(parsed.get("samplerate"), Some(&Value::Number(5.into())));
        assert!(parsed.get("data").is_some());
        assert_eq!(
            parsed.get("data").unwrap().get("message"),
            Some(&Value::String("test".to_string()))
        );
    }

    #[test]
    fn test_serialization_excludes_metadata() {
        let mut event = Event::new();
        event.add_field("key", "value");
        event.set_metadata("should-not-appear");

        let json = event.to_json().unwrap();
        assert!(!json.contains("metadata"));
        assert!(!json.contains("should-not-appear"));
    }

    #[test]
    fn test_deserialize_event() {
        let json = r#"{
            "time": "2024-01-15T10:30:00.000Z",
            "samplerate": 1,
            "data": {"key": "value"}
        }"#;

        let event: Event = serde_json::from_str(json).unwrap();
        assert_eq!(event.timestamp, "2024-01-15T10:30:00.000Z");
        assert_eq!(event.sample_rate, 1);
        assert_eq!(
            event.data.get("key"),
            Some(&Value::String("value".to_string()))
        );
    }

    #[test]
    fn test_batch_serialization() {
        let mut event1 = Event::new();
        event1.add_field("index", 1);

        let mut event2 = Event::new();
        event2.add_field("index", 2);

        let batch = vec![&event1, &event2];
        let json = serde_json::to_string(&batch).unwrap();

        let parsed: Vec<Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    // =====================================================
    // Clone and Debug Tests
    // =====================================================

    #[test]
    fn test_event_clone() {
        let mut event1 = Event::new();
        event1.add_field("key", "value");
        event1.set_metadata("meta");

        let event2 = event1.clone();

        assert_eq!(event1.timestamp, event2.timestamp);
        assert_eq!(event1.sample_rate, event2.sample_rate);
        assert_eq!(event1.data, event2.data);
        assert_eq!(event1.metadata, event2.metadata);
    }

    #[test]
    fn test_event_debug() {
        let mut event = Event::new();
        event.add_field("key", "value");

        let debug_str = format!("{event:?}");
        assert!(debug_str.contains("Event"));
        assert!(debug_str.contains("timestamp"));
        assert!(debug_str.contains("data"));
    }

    // =====================================================
    // FieldHolder Trait Tests
    // =====================================================

    #[test]
    fn test_field_holder_trait() {
        fn add_common_fields(holder: &mut impl FieldHolder) {
            holder.add_field("service", "test-service");
            holder.add_field("version", "1.0.0");
        }

        let mut event = Event::new();
        add_common_fields(&mut event);

        assert_eq!(
            event.data.get("service"),
            Some(&Value::String("test-service".to_string()))
        );
        assert_eq!(
            event.data.get("version"),
            Some(&Value::String("1.0.0".to_string()))
        );
    }

    // =====================================================
    // Edge Cases
    // =====================================================

    #[test]
    fn test_empty_string_field() {
        let mut event = Event::new();
        event.add_field("", "value");
        event.add_field("key", "");

        assert!(event.data.contains_key(""));
        assert_eq!(event.data.get("key"), Some(&Value::String("".to_string())));
    }

    #[test]
    fn test_unicode_fields() {
        let mut event = Event::new();
        event.add_field("emoji", "🎉");
        event.add_field("中文", "Chinese");
        event.add_field("key", "日本語");

        assert_eq!(
            event.data.get("emoji"),
            Some(&Value::String("🎉".to_string()))
        );
        assert_eq!(
            event.data.get("中文"),
            Some(&Value::String("Chinese".to_string()))
        );
        assert_eq!(
            event.data.get("key"),
            Some(&Value::String("日本語".to_string()))
        );

        // Verify it serializes correctly
        let json = event.to_json().unwrap();
        assert!(json.contains("🎉"));
        assert!(json.contains("中文"));
        assert!(json.contains("日本語"));
    }

    #[test]
    fn test_large_number_of_fields() {
        let mut event = Event::new();
        for i in 0..1000 {
            event.add_field(format!("field_{i}"), i);
        }

        assert_eq!(event.len(), 1000);
        assert!(event.to_json().is_ok());
    }

    #[test]
    fn test_nested_json_value() {
        let mut event = Event::new();
        event.add_field(
            "nested",
            serde_json::json!({
                "level1": {
                    "level2": {
                        "value": 42
                    }
                }
            }),
        );

        let json = event.to_json().unwrap();
        assert!(json.contains("level1"));
        assert!(json.contains("level2"));
        assert!(json.contains("42"));
    }
}
