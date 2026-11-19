//! M6 tests — the tool-schema contract (architecture §4.4).
//!
//! These exist so that a careless edit to `agent-schema/v1/tools.json` fails
//! the build instead of silently invalidating every checkpoint trained against
//! it.

use agent_schema::{check_checkpoint, schema, Compatibility, Version};

#[test]
fn embedded_contract_parses() {
    // If this fails, every consumer of the contract is broken and the panic in
    // `schema()` would otherwise surface at runtime instead of in CI.
    let s = schema();
    assert!(!s.tools.is_empty());
    assert_eq!(s.version(), Version { major: 1, minor: 0 });
}

#[test]
fn contract_covers_exactly_the_reference_runtime_toolset() {
    // v1 is pinned to pi's built-in toolset. A tool appearing or vanishing here
    // is a schema-version event, not an incidental edit — that is the whole
    // point of the contract.
    let s = schema();
    let names = s.tool_names();
    let expected: std::collections::BTreeSet<&str> =
        ["bash", "edit", "find", "grep", "ls", "read", "write"]
            .into_iter()
            .collect();
    assert_eq!(names, expected);
}

#[test]
fn every_tool_declares_a_usable_json_schema() {
    for tool in schema().tools {
        assert!(
            !tool.description.is_empty(),
            "{} has no description",
            tool.name
        );
        assert_eq!(
            tool.parameters.get("type").and_then(|t| t.as_str()),
            Some("object"),
            "{}'s parameters must be a JSON Schema object",
            tool.name
        );
        assert!(
            tool.parameters.get("properties").is_some(),
            "{} declares no properties",
            tool.name
        );
    }
}

#[test]
fn required_params_are_declared_properties() {
    // A `required` naming something absent from `properties` is the kind of
    // drift that produces a model emitting arguments the runtime rejects.
    for tool in schema().tools {
        let props = tool.parameters["properties"].as_object().unwrap();
        let Some(required) = tool.parameters.get("required").and_then(|r| r.as_array()) else {
            continue;
        };
        for name in required {
            let name = name.as_str().unwrap();
            assert!(
                props.contains_key(name),
                "{}: required param {name:?} is not in properties",
                tool.name
            );
        }
    }
}

#[test]
fn provenance_records_which_agent_build_the_contract_came_from() {
    // Training records carry this; without it a checkpoint's toolset cannot be
    // traced back to the runtime that produced its data.
    let p = schema().generated_from;
    assert!(p.agent.contains("pi-coding-agent"));
    // A concrete pin, not "latest" — the contract must name the build it came from.
    assert!(
        p.version.split('.').count() == 3 && p.version.chars().next().unwrap().is_ascii_digit(),
        "expected a pinned x.y.z agent version, got {:?}",
        p.version
    );
    assert!(!p.dumped_at.is_empty());
}

// ---- checkpoint compatibility (§4.4, R5) -----------------------------------

#[test]
fn matching_checkpoint_is_exact() {
    assert_eq!(check_checkpoint("1.0"), Compatibility::Exact);
    assert!(check_checkpoint("1.0").is_usable());
}

#[test]
fn older_minor_is_usable_and_says_why() {
    // Trained on 1.0, harness ships 1.1: every tool it knows still exists.
    let v = Compatibility::CompatibleOlderMinor {
        checkpoint: Version { major: 1, minor: 0 },
        current: Version { major: 1, minor: 1 },
    };
    assert!(v.is_usable());
    assert!(v.to_string().contains("will not use tools added"));
}

#[test]
fn newer_minor_is_usable_but_flagged() {
    let v = Compatibility::CompatibleNewerMinor {
        checkpoint: Version { major: 1, minor: 2 },
        current: Version { major: 1, minor: 0 },
    };
    assert!(v.is_usable());
    assert!(v
        .to_string()
        .contains("may call tools this build does not provide"));
}

#[test]
fn major_mismatch_is_refused() {
    let v = check_checkpoint("2.0");
    assert!(!v.is_usable(), "a breaking toolset change must be refused");
    assert!(matches!(v, Compatibility::Incompatible { .. }));
}

#[test]
fn undeclared_or_garbage_version_is_refused() {
    // An undeclared schema is precisely what the contract exists to prevent,
    // so it must fail closed rather than optimistically run.
    for declared in ["", "latest", "v1", "1", "1.x", "abc"] {
        let v = check_checkpoint(declared);
        assert!(
            !v.is_usable(),
            "{declared:?} should not be treated as a usable declaration"
        );
    }
}

#[test]
fn version_parsing_round_trips() {
    assert_eq!(
        Version::parse("3.14"),
        Some(Version {
            major: 3,
            minor: 14
        })
    );
    assert_eq!(Version::parse("3.14").unwrap().to_string(), "3.14");
    assert_eq!(
        Version::parse("  1.0  "),
        Some(Version { major: 1, minor: 0 })
    );
}
