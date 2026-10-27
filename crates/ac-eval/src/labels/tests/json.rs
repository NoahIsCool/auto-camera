//! Tests for reading and writing label files: the canonical form, the
//! version check that comes first, and shape and rule errors with paths.

use serde_json::{Value, json};

use super::*;

/// The sample file as a JSON value, for editing.
fn sample_value() -> Value {
    serde_json::from_str(&sample().to_json()).unwrap()
}

/// Reads `value` as a label file.
fn read(value: &Value) -> Result<LabelFile, FieldErrors> {
    LabelFile::from_json(&value.to_string())
}

/// Asserts that reading `text` fails with exactly one error, at `path`,
/// with `words` in its message.
#[track_caller]
fn assert_unreadable(text: &str, path: &str, words: &str) {
    let errors = LabelFile::from_json(text).expect_err("the text is not a valid file");
    assert_eq!(errors.paths(), [path], "{errors}");
    assert!(errors.0[0].message.contains(words), "{errors}");
}

#[test]
fn written_files_read_back_and_rewrite_unchanged() {
    let text = sample().to_json();
    assert!(text.ends_with("}\n"));
    let labels = LabelFile::from_json(&text).unwrap();
    assert_eq!(labels, sample());
    assert_eq!(labels.to_json(), text);
}

#[test]
fn empty_truncation_and_lines_are_left_out() {
    let value = sample_value();
    let people = &value["keyframes"][0]["people"];
    // The guitarist has neither; the pastor's truncation names only the
    // cut side.
    assert_eq!(people[1].get("truncation"), None);
    assert_eq!(people[1].get("lines"), None);
    assert_eq!(people[0]["truncation"], json!({ "bottom": true }));
    assert_eq!(
        people[0]["lines"],
        json!({ "crown": 0.25, "chin": 0.3125, "shoulder": 0.375, "waist": 0.5 })
    );
}

#[test]
fn uncut_sides_and_unlabelled_lines_may_be_written_out() {
    let mut value = sample_value();
    let guitarist = &mut value["keyframes"][0]["people"][1];
    guitarist["truncation"] = json!({ "top": false, "right": false });
    guitarist["lines"] = json!({});
    assert_eq!(read(&value), Ok(sample()));
}

#[test]
fn a_missing_schema_version_is_named() {
    let mut value = sample_value();
    value.as_object_mut().unwrap().remove("schema_version");
    assert_unreadable(&value.to_string(), "schema_version", "missing");

    value["schema_version"] = Value::Null;
    assert_unreadable(&value.to_string(), "schema_version", "missing");

    // The version is checked before the shape, so a file of another shape
    // is reported by its missing version alone.
    assert_unreadable(r#"{"tracks": 5}"#, "schema_version", "missing");
}

#[test]
fn only_this_builds_version_is_read() {
    for (version, words) in [
        (
            json!(2),
            "version 2 is not supported; this build reads version 1",
        ),
        (json!(0), "version 0 is not supported"),
        (json!("1"), "\"1\" is not a version number"),
        (json!(1.5), "1.5 is not a version number"),
        (json!(-1), "-1 is not a version number"),
    ] {
        let mut value = sample_value();
        value["schema_version"] = version;
        assert_unreadable(&value.to_string(), "schema_version", words);
    }
}

#[test]
fn text_that_is_not_a_json_object_is_rejected_at_the_root() {
    assert_unreadable("", ".", "not a label file");
    assert_unreadable("{\"schema_version\": 1,", ".", "line 1");
    assert_unreadable("\"labels\"", ".", "not a label file");
    let mut text = sample().to_json();
    text.push_str("{}");
    assert_unreadable(&text, ".", "trailing characters");
}

#[test]
fn shape_errors_name_the_field() {
    let mut value = sample_value();
    value["keyframes"][1]["people"][0]
        .as_object_mut()
        .unwrap()
        .remove("visibility");
    assert_unreadable(
        &value.to_string(),
        "keyframes[1].people[0]",
        "missing field `visibility`",
    );

    let mut value = sample_value();
    value["keyframes"][0]["frame"] = json!("zero");
    assert_unreadable(&value.to_string(), "keyframes[0].frame", "invalid type");

    let mut value = sample_value();
    value["roles"][1]["start_frame"] = json!(-3);
    assert_unreadable(&value.to_string(), "roles[1].start_frame", "invalid value");
}

#[test]
fn misspelt_fields_are_errors_not_defaults() {
    let mut value = sample_value();
    value["keyframes"][0]["people"][1]["truncaton"] = json!({ "bottom": true });
    assert_unreadable(
        &value.to_string(),
        "keyframes[0].people[1].truncaton",
        "unknown field `truncaton`",
    );

    let mut value = sample_value();
    value["keyframes"][0]["people"][0]["lines"]["shoulders"] = json!(0.4);
    assert_unreadable(
        &value.to_string(),
        "keyframes[0].people[0].lines.shoulders",
        "unknown field `shoulders`",
    );
}

#[test]
fn rule_errors_follow_a_good_shape() {
    let mut value = sample_value();
    value["keyframes"][0]["people"][1]["bbox"]["x"] = json!(-0.5);
    value["roles"][0]["track"] = json!(8);
    let errors = read(&value).unwrap_err();
    assert_eq!(
        errors.paths(),
        ["roles[0].track", "keyframes[0].people[1].bbox.x"]
    );
}
