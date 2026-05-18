use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ── Test harness ──────────────────────────────────────────────────────────────

fn aizo(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("aizo").unwrap();
    cmd.env("AIZO_DB_PATH", tmp.path().join("test.db"))
       .env("HOME", tmp.path()); // isolates ~/.aizo/scenarios.yaml creation
    cmd
}

fn add(tmp: &TempDir, item: &str, reason: &str, score: f64) {
    aizo(tmp)
        .args(["add", item, reason, "--score", &score.to_string()])
        .assert().success();
}

fn show_json(tmp: &TempDir) -> Vec<serde_json::Value> {
    let out = aizo(tmp).args(["show", "--format", "json"])
        .assert().success()
        .get_output().stdout.clone();
    serde_json::from_slice(&out).unwrap()
}

// ── add ───────────────────────────────────────────────────────────────────────

#[test]
fn add_and_show() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "concise code", "likes short code", 9.0);
    aizo(&tmp).args(["show"])
        .assert().success()
        .stdout(predicate::str::contains("concise code"));
}

#[test]
fn add_score_out_of_range_fails() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["add", "item", "reason", "--score", "11.0"])
        .assert().failure();
    aizo(&tmp).args(["add", "item", "reason", "--score", "-1.0"])
        .assert().failure();
}

#[test]
fn add_default_score_is_9() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["add", "item", "reason"]).assert().success();
    let prefs = show_json(&tmp);
    assert!((prefs[0]["base_score"].as_f64().unwrap() - 9.0).abs() < f64::EPSILON);
}

#[test]
fn add_duplicate_applies_smoothing() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "reason", 8.0);
    add(&tmp, "item", "reason", 10.0);
    let prefs = show_json(&tmp);
    assert_eq!(prefs.len(), 1);
    let score = prefs[0]["base_score"].as_f64().unwrap();
    let expected = 8.0_f64.mul_add(0.4, 10.0 * 0.6);
    assert!((score - expected).abs() < 0.001, "score={score} expected={expected}");
}

#[test]
fn add_with_keywords_and_scenarios() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["add", "concise code", "likes brevity",
                     "--keywords", "brevity,minimal",
                     "--scenarios", "coding",
                     "--score", "9.0"])
        .assert().success();
    let prefs = show_json(&tmp);
    assert_eq!(prefs[0]["keywords"].as_array().unwrap().len(), 2);
    assert_eq!(prefs[0]["scenarios"][0].as_str().unwrap(), "coding");
}

// ── recall ────────────────────────────────────────────────────────────────────

#[test]
fn recall_keyword_match() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "concise code", "likes brevity", 9.0);
    add(&tmp, "dark mode", "uses dark theme", 5.0);
    aizo(&tmp).args(["recall", "concise"]).assert().success()
        .stdout(predicate::str::contains("concise code"))
        .stdout(predicate::str::contains("dark mode").not());
}

#[test]
fn recall_no_match_prints_message() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "reason", 5.0);
    aizo(&tmp).args(["recall", "zzznomatch"])
        .assert().success()
        .stdout(predicate::str::contains("No preferences matched"));
}

#[test]
fn recall_no_match_json_returns_empty_array() {
    let tmp = TempDir::new().unwrap();
    let out = aizo(&tmp).args(["recall", "zzz", "--format", "json"])
        .assert().success()
        .get_output().stdout.clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[test]
fn recall_type_taboo_only() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "emojis",  "never use",  0.5);
    add(&tmp, "verbose", "dislikes",   2.5);
    add(&tmp, "dark",    "neutral",    5.0);
    add(&tmp, "concise", "loves this", 9.0);
    aizo(&tmp).args(["recall", "--type", "taboo"]).assert().success()
        .stdout(predicate::str::contains("emojis"))
        .stdout(predicate::str::contains("verbose").not())
        .stdout(predicate::str::contains("concise").not());
}

#[test]
fn recall_multiple_types() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "emojis",  "never",  0.5);
    add(&tmp, "dark",    "neutral", 5.0);
    add(&tmp, "concise", "loves",   9.0);
    let out = aizo(&tmp).args(["recall", "--type", "taboo,preference", "--format", "json"])
        .assert().success()
        .get_output().stdout.clone();
    let prefs: Vec<serde_json::Value> = serde_json::from_slice(&out).unwrap();
    assert_eq!(prefs.len(), 2);
}

#[test]
fn recall_min_score_filter() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "high", "reason", 9.0);
    add(&tmp, "low",  "reason", 2.0);
    aizo(&tmp).args(["recall", "--min-score", "7.0"]).assert().success()
        .stdout(predicate::str::contains("high"))
        .stdout(predicate::str::contains("low").not());
}

#[test]
fn recall_limit() {
    let tmp = TempDir::new().unwrap();
    for i in 0..10 { add(&tmp, &format!("item{i}"), "r", 5.0); }
    let out = aizo(&tmp).args(["recall", "--limit", "3", "--format", "json"])
        .assert().success()
        .get_output().stdout.clone();
    let prefs: Vec<serde_json::Value> = serde_json::from_slice(&out).unwrap();
    assert_eq!(prefs.len(), 3);
}

#[test]
fn recall_unknown_type_fails() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["recall", "--type", "invalid_band"]).assert().failure();
}

#[test]
fn recall_empty_db_no_crash() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["recall", "anything"]).assert().success();
}

// ── top ───────────────────────────────────────────────────────────────────────

#[test]
fn top_n_limits_output() {
    let tmp = TempDir::new().unwrap();
    for i in 0..10 { add(&tmp, &format!("item{i}"), "r", 5.0); }
    let out = aizo(&tmp).args(["top", "3", "--format", "json"])
        .assert().success()
        .get_output().stdout.clone();
    let prefs: Vec<serde_json::Value> = serde_json::from_slice(&out).unwrap();
    assert_eq!(prefs.len(), 3);
}

#[test]
fn top_empty_returns_message() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["top", "10"]).assert().success()
        .stdout(predicate::str::contains("No preferences"));
}

#[test]
fn top_empty_json_returns_empty_array() {
    let tmp = TempDir::new().unwrap();
    let out = aizo(&tmp).args(["top", "10", "--format", "json"])
        .assert().success()
        .get_output().stdout.clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[test]
fn top_type_filter() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "taboo item", "hard limit", 0.5);
    add(&tmp, "pref item",  "loves this", 9.0);
    aizo(&tmp).args(["top", "10", "--type", "taboo"]).assert().success()
        .stdout(predicate::str::contains("taboo item"))
        .stdout(predicate::str::contains("pref item").not());
}

// ── show ──────────────────────────────────────────────────────────────────────

#[test]
fn show_json_valid() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "reason", 5.0);
    let prefs = show_json(&tmp);
    assert_eq!(prefs.len(), 1);
    assert_eq!(prefs[0]["item"].as_str().unwrap(), "item");
}

#[test]
fn show_json_flag_backward_compat() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "reason", 5.0);
    let out = aizo(&tmp).args(["show", "--json"])
        .assert().success()
        .get_output().stdout.clone();
    let prefs: Vec<serde_json::Value> = serde_json::from_slice(&out).unwrap();
    assert!(!prefs.is_empty());
}

#[test]
fn show_format_context_structure() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "concise",  "loves brevity", 9.0);
    add(&tmp, "dark mode","neutral habit",  5.0);
    add(&tmp, "verbose",  "dislikes",       2.0);
    add(&tmp, "emojis",   "hard limit",     0.5);
    aizo(&tmp).args(["show", "--format", "context"]).assert().success()
        .stdout(predicate::str::contains("[User Preferences]"))
        .stdout(predicate::str::contains("Loves:"))
        .stdout(predicate::str::contains("Habits:"))
        .stdout(predicate::str::contains("Dislikes:"))
        .stdout(predicate::str::contains("Hard limits:"));
}

#[test]
fn show_score_band_filter() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "taboo item", "hard limit", 0.5);
    add(&tmp, "pref item",  "loves this", 9.0);
    aizo(&tmp).args(["show", "--type", "taboo"]).assert().success()
        .stdout(predicate::str::contains("taboo item"))
        .stdout(predicate::str::contains("pref item").not());
}

#[test]
fn show_empty_db_no_crash() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["show"]).assert().success()
        .stdout(predicate::str::contains("No preferences"));
}

#[test]
fn show_empty_json_returns_empty_array() {
    let tmp = TempDir::new().unwrap();
    let out = aizo(&tmp).args(["show", "--format", "json"])
        .assert().success()
        .get_output().stdout.clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[test]
fn recall_format_context_groups_correctly() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "concise", "loves",      9.0);
    add(&tmp, "emojis",  "hard limit", 0.5);
    aizo(&tmp).args(["recall", "--format", "context"]).assert().success()
        .stdout(predicate::str::contains("[User Preferences]"))
        .stdout(predicate::str::contains("concise"))
        .stdout(predicate::str::contains("emojis"));
}

// ── update ────────────────────────────────────────────────────────────────────

#[test]
fn update_score() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "reason", 5.0);
    aizo(&tmp).args(["update", "item", "--score", "9.0"]).assert().success();
    let prefs = show_json(&tmp);
    assert!((prefs[0]["base_score"].as_f64().unwrap() - 9.0).abs() < f64::EPSILON);
}

#[test]
fn update_reason() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "old reason", 5.0);
    aizo(&tmp).args(["update", "item", "--reason", "new reason"]).assert().success();
    let prefs = show_json(&tmp);
    assert_eq!(prefs[0]["reason"].as_str().unwrap(), "new reason");
}

#[test]
fn update_no_fields_fails() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "reason", 5.0);
    aizo(&tmp).args(["update", "item"]).assert().failure();
}

#[test]
fn update_not_found_prints_message() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["update", "nonexistent", "--score", "5.0"]).assert().success()
        .stdout(predicate::str::contains("Not found"));
}

// ── touch ─────────────────────────────────────────────────────────────────────

#[test]
fn touch_existing_entry() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "reason", 5.0);
    aizo(&tmp).args(["touch", "item"]).assert().success()
        .stdout(predicate::str::contains("Touched"));
}

#[test]
fn touch_not_found_prints_message() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["touch", "nonexistent"]).assert().success()
        .stdout(predicate::str::contains("Not found"));
}

// ── remove ────────────────────────────────────────────────────────────────────

#[test]
fn remove_deletes_entry() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "reason", 5.0);
    aizo(&tmp).args(["remove", "item"]).assert().success();
    aizo(&tmp).args(["show"]).assert().success()
        .stdout(predicate::str::contains("No preferences"));
}

#[test]
fn remove_not_found_prints_message() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["remove", "nonexistent"]).assert().success()
        .stdout(predicate::str::contains("Not found"));
}

// ── export ────────────────────────────────────────────────────────────────────

#[test]
fn export_stdout_valid_json() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "reason", 5.0);
    let out = aizo(&tmp).args(["export"])
        .assert().success()
        .get_output().stdout.clone();
    let val: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(val["entries"].as_array().unwrap().len(), 1);
    assert_eq!(val["entries"][0]["item"].as_str().unwrap(), "item");
}

#[test]
fn export_includes_keywords_and_scenarios() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["add", "item", "reason",
                     "--keywords", "k1,k2", "--scenarios", "coding"])
        .assert().success();
    let out = aizo(&tmp).args(["export"])
        .assert().success()
        .get_output().stdout.clone();
    let val: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let entry = &val["entries"][0];
    assert_eq!(entry["keywords"].as_array().unwrap().len(), 2);
    assert_eq!(entry["scenarios"][0].as_str().unwrap(), "coding");
}

#[test]
fn export_to_file() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "reason", 5.0);
    let out_path = tmp.path().join("export.json");
    aizo(&tmp).args(["export", "--output", out_path.to_str().unwrap()])
        .assert().success()
        .stdout(predicate::str::contains("Exported 1 entries"));
    let content = std::fs::read_to_string(&out_path).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(val["entries"].as_array().unwrap().len(), 1);
}

#[test]
fn export_empty_db() {
    let tmp = TempDir::new().unwrap();
    let out = aizo(&tmp).args(["export"])
        .assert().success()
        .get_output().stdout.clone();
    let val: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(val["entries"].as_array().unwrap().len(), 0);
}

// ── keywords ──────────────────────────────────────────────────────────────────

#[test]
fn keywords_shows_stored() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["add", "item", "reason", "--keywords", "brevity,minimal"])
        .assert().success();
    aizo(&tmp).args(["keywords"]).assert().success()
        .stdout(predicate::str::contains("brevity"))
        .stdout(predicate::str::contains("minimal"));
}

#[test]
fn keywords_empty_db() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["keywords"]).assert().success()
        .stdout(predicate::str::contains("No keywords"));
}

// ── config ────────────────────────────────────────────────────────────────────

#[test]
fn config_set_half_life() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["config", "set-half-life", "14"]).assert().success();
    let out = aizo(&tmp).args(["config", "show"])
        .assert().success()
        .get_output().stdout.clone();
    let cfg: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!((cfg["half_life_days"].as_f64().unwrap() - 14.0).abs() < f64::EPSILON);
}

#[test]
fn config_set_floor() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["config", "set-floor", "0.05"]).assert().success();
    let out = aizo(&tmp).args(["config", "show"])
        .assert().success()
        .get_output().stdout.clone();
    let cfg: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!((cfg["floor"].as_f64().unwrap() - 0.05).abs() < f64::EPSILON);
}

#[test]
fn config_floor_out_of_range_fails() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["config", "set-floor", "1.5"]).assert().failure();
    aizo(&tmp).args(["config", "set-floor", "-0.1"]).assert().failure();
}

#[test]
fn config_half_life_zero_fails() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["config", "set-half-life", "0"]).assert().failure();
}

// ── info ──────────────────────────────────────────────────────────────────────

#[test]
fn info_shows_key_fields() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["info"]).assert().success()
        .stdout(predicate::str::contains("Database"))
        .stdout(predicate::str::contains("Total"))
        .stdout(predicate::str::contains("Decay"));
}

// ── clear ─────────────────────────────────────────────────────────────────────

#[test]
fn clear_wipes_all() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "a", "r", 5.0);
    add(&tmp, "b", "r", 7.0);
    aizo(&tmp).args(["clear"]).assert().success();
    let prefs = show_json(&tmp);
    assert_eq!(prefs.len(), 0);
}

// ── edge cases ────────────────────────────────────────────────────────────────

#[test]
fn score_band_aliases_all_accepted() {
    let tmp = TempDir::new().unwrap();
    for band in &["preference", "pref", "style", "habit", "neutral",
                  "aversion", "dislike", "taboo", "limit"] {
        aizo(&tmp).args(["recall", "--type", band])
            .assert().success();
    }
}

#[test]
fn items_with_spaces_handled() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "multi word item", "this has spaces", 7.0);
    aizo(&tmp).args(["touch", "multi word item"]).assert().success()
        .stdout(predicate::str::contains("Touched"));
    aizo(&tmp).args(["remove", "multi word item"]).assert().success()
        .stdout(predicate::str::contains("Removed"));
}

#[test]
fn effective_weight_in_json_output() {
    let tmp = TempDir::new().unwrap();
    add(&tmp, "item", "reason", 8.0);
    let prefs = show_json(&tmp);
    let w = prefs[0]["effective_weight"].as_f64().unwrap();
    // At t=0, effective_weight ≈ base_score
    assert!(w > 0.0 && w <= 8.0 + 0.001);
}

#[test]
fn scenario_field_present_in_json() {
    let tmp = TempDir::new().unwrap();
    aizo(&tmp).args(["add", "item", "reason", "--scenarios", "coding"])
        .assert().success();
    let prefs = show_json(&tmp);
    assert!(prefs[0]["scenarios"].is_array());
}
