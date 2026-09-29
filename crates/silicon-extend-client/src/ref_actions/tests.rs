use super::provider::decode_for_test;
use super::*;

fn commands() -> Vec<String> {
    ["snapshot", "click", "fill", "focus", "get"]
        .map(str::to_owned)
        .to_vec()
}
fn screen() -> Value {
    json!({"nodes":[
        {"ref":"e1","type":"Button","label":"Save","enabled":true},
        {"ref":"e2","type":"TextField","label":"Email","value":"","enabled":true},
        {"ref":"e3","type":"Button","label":"Cancel","enabled":true}
    ]})
}
fn observation() -> Observation {
    Observation::from_snapshot(&screen(), &commands(), true).unwrap()
}
fn jev_answer(criteria: &Value, selected: &str) -> Value {
    let p: Map<String, Value> = criteria
        .as_object()
        .unwrap()
        .keys()
        .map(|k| (k.clone(), json!(if k == selected { 1.0 } else { 0.0 })))
        .collect();
    json!({"choice":selected,"confidence":1.0,"probabilities":p})
}
fn response(o: &Observation, operation: &str, target: &str) -> Value {
    let body = o.request("click Save").unwrap();
    let mut answers = Map::new();
    answers.insert(
        "operation".into(),
        jev_answer(&body["questions"]["operation"]["criteria"], operation),
    );
    if operation != "BLOCKED" {
        let key = format!("{operation}_target");
        answers.insert(key.clone(), jev_answer(&body["questions"][&key]["criteria"], target));
    }
    json!({"model":"jev-test","answers":answers})
}

#[test]
fn nested_android_and_flat_engine_snapshots_offer_observed_refs() {
    for platform in [
        "android",
        "android_tv",
        "fire_os",
        "ios",
        "ipados",
        "macos",
        "linux",
        "windows",
    ] {
        let mut s = screen();
        s["platform"] = json!(platform);
        if matches!(platform, "android" | "android_tv" | "fire_os") {
            let children = s["nodes"].take();
            s["nodes"] = json!([{"children":children}]);
        }
        let o = Observation::from_snapshot(&s, &commands(), true).unwrap();
        assert_eq!(o.elements.len(), 3, "{platform}");
        assert_eq!(o.targets["fill"], BTreeSet::from(["@e2".into()]));
        let d = decode_for_test(Provider::Jev, &o, &response(&o, "click", "@e1")).unwrap();
        assert_eq!(o.command(&d, None).unwrap(), ("click".into(), vec!["@e1".into()]));
    }
}

#[test]
fn disabled_hidden_and_password_nodes_are_not_sent_to_the_model() {
    let s = json!({"nodes":[
        {"ref":"@e1","label":"Save"},
        {"ref":"e2","enabled":false,"label":"disabled"},
        {"ref":"e3","hittable":false,"label":"covered"},
        {"ref":"e4","password":true,"value":"SECRET","children":[{"ref":"e5","text":"SECRET CHILD"}]},
        {"ref":"e6","type":"SecureTextField","value":"SECRET"}
    ]});
    let o = Observation::from_snapshot(&s, &commands(), true).unwrap();
    assert_eq!(o.elements.len(), 1);
    assert!(!o.request("Click Save").unwrap().to_string().contains("SECRET"));
}

#[test]
fn capabilities_and_exact_text_limit_the_action_space() {
    let o = Observation::from_snapshot(&screen(), &["get".into()], false).unwrap();
    assert_eq!(o.targets.keys().cloned().collect::<Vec<_>>(), vec!["get_text"]);
    let o = Observation::from_snapshot(&screen(), &commands(), false).unwrap();
    assert!(!o.targets.contains_key("fill"));
    assert!(Observation::from_snapshot(&screen(), &["tv-remote".into()], false).is_err());
    assert!(Observation::from_snapshot(&json!({"image":"screenshot"}), &commands(), false).is_err());
}

#[test]
fn refuses_duplicate_malformed_and_overflow_refs_without_truncation() {
    for refs in [vec!["e1", "e1"], vec!["e1", "--help"], vec!["e1", "e2;evil"]] {
        let nodes: Vec<_> = refs.iter().map(|r| json!({"ref":r})).collect();
        assert!(Observation::from_snapshot(&json!({"nodes":nodes}), &commands(), false).is_err());
    }
    let nodes: Vec<_> = (1..=255).map(|r| json!({"ref":format!("e{r}")})).collect();
    assert!(Observation::from_snapshot(&json!({"nodes":nodes}), &commands(), false).is_err());
}

#[test]
fn only_the_chosen_head_can_execute_and_no_match_abstains() {
    let o = observation();
    let mut r = response(&o, "click", "@e1");
    r["answers"]["fill_target"] = json!({"choice":"evil","probabilities":{}});
    assert!(decode_for_test(Provider::Jev, &o, &r).unwrap().accepted);
    for (op, target) in [("BLOCKED", ""), ("click", "BLOCKED")] {
        let d = decode_for_test(Provider::Jev, &o, &response(&o, op, target)).unwrap();
        assert!(!d.accepted);
        assert!(o.command(&d, None).is_err());
    }
}

#[test]
fn malformed_probabilities_and_unknown_choices_never_execute() {
    let o = observation();
    let good = response(&o, "click", "@e1");
    let mut bad = good.clone();
    bad["answers"]["click_target"]["choice"] = json!("@e999");
    assert!(decode_for_test(Provider::Jev, &o, &bad).is_err());
    let mut bad = good.clone();
    bad["answers"]["operation"]["probabilities"]["click"] = json!(2.0);
    assert!(decode_for_test(Provider::Jev, &o, &bad).is_err());
    let mut bad = good.clone();
    bad["answers"]["operation"]["probabilities"]["extra"] = json!(0.0);
    assert!(decode_for_test(Provider::Jev, &o, &bad).is_err());
    let mut low = good;
    low["answers"]["click_target"]["confidence"] = json!(0.4);
    assert!(!decode_for_test(Provider::Jev, &o, &low).unwrap().accepted);
}

#[test]
fn llm_baseline_uses_identical_refs_and_fill_never_generates_arguments() {
    let o = observation();
    let response = |operation: &str, target: &str| json!({"choices":[{"message":{"content":json!({"operation":operation,"target":target}).to_string()}}]});
    let d = decode_for_test(Provider::Llm, &o, &response("fill", "@e2")).unwrap();
    assert_eq!(
        o.command(&d, Some("literal text with --json")).unwrap(),
        ("fill".into(), vec!["@e2".into(), "literal text with --json".into()])
    );
    assert!(o.command(&d, Some("--json")).is_err());
    assert!(o.command(&d, None).is_err());
    assert!(decode_for_test(Provider::Llm, &o, &response("fill", "@e1")).is_err());
    assert!(decode_for_test(Provider::Llm, &o, &response("shell", "@e1")).is_err());
}

#[test]
fn freshness_comparison_catches_changed_values_labels_and_geometry() {
    let original = observation();
    for (field, value) in [
        ("label", json!("Delete")),
        ("value", json!("changed")),
        ("rect", json!({"x":100,"y":200,"width":40,"height":30})),
    ] {
        let mut changed = screen();
        changed["nodes"][0][field] = value;
        assert_ne!(
            original,
            Observation::from_snapshot(&changed, &commands(), true).unwrap()
        );
    }
}
