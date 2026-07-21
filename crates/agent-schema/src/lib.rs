//! agent-schema — the tool-schema contract (architecture §4.4).
//!
//! `agent-schema/v1/tools.json` at the repo root is the **single source of
//! truth** for the toolset the reference runtime exposes and the training data
//! is generated against. Three consumers import it and must never drift:
//!
//! 1. the pi extension (`pi-ext/harness-tools.ts`) — makes the *runtime*
//!    toolset exactly the schema, via `setActiveTools`;
//! 2. the training generator (`training/`) — emits records shaped by it;
//! 3. this crate — lets the harness check a model checkpoint's declared
//!    version before handing it work.
//!
//! The JSON is embedded with `include_str!`, so a packaged
//! `openOrchestra.app` carries the contract with it and cannot fall out of step
//! with the binary it shipped in.
//!
//! ## Versioning
//!
//! `schemaVersion` is `MAJOR.MINOR`:
//!
//! - **MINOR** bumps are additive and backward compatible — a new tool, a new
//!   optional parameter. A checkpoint trained on `1.0` still works on `1.1`; it
//!   simply never emits the tools it has not seen.
//! - **MAJOR** bumps are breaking — a tool removed or renamed, a parameter's
//!   type or meaning changed, a required parameter added. A checkpoint trained
//!   on a different major will call tools that no longer exist, or call them
//!   wrongly, so the harness refuses (§4.4, R5).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The embedded contract. Parsed lazily by [`schema`].
pub const TOOLS_JSON: &str = include_str!("../../../agent-schema/v1/tools.json");

/// One tool in the contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments, verbatim as the agent declares it.
    pub parameters: serde_json::Value,
}

/// Where the contract was derived from, for provenance in training records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub agent: String,
    pub version: String,
    pub dumped_at: String,
}

/// The parsed contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSchema {
    pub schema_version: String,
    pub generated_from: Provenance,
    pub tools: Vec<ToolSchema>,
}

impl AgentSchema {
    pub fn tool(&self, name: &str) -> Option<&ToolSchema> {
        self.tools.iter().find(|t| t.name == name)
    }

    pub fn tool_names(&self) -> BTreeSet<&str> {
        self.tools.iter().map(|t| t.name.as_str()).collect()
    }

    pub fn version(&self) -> Version {
        Version::parse(&self.schema_version).expect("embedded schema version is well-formed")
    }
}

/// Parse the embedded contract.
///
/// Panics only if the embedded JSON is malformed, which a test in this crate
/// makes a build-time failure rather than a runtime one.
pub fn schema() -> AgentSchema {
    serde_json::from_str(TOOLS_JSON).expect("embedded agent-schema/v1/tools.json is valid")
}

/// A `MAJOR.MINOR` schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let (major, minor) = s.trim().split_once('.')?;
        Some(Version {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The verdict on a checkpoint's declared schema version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compatibility {
    /// Trained against exactly this contract.
    Exact,
    /// Trained against an older minor of the same major: every tool the
    /// checkpoint knows still exists and still means the same thing. It just
    /// will not use whatever was added since.
    CompatibleOlderMinor { checkpoint: Version, current: Version },
    /// Trained against a *newer* minor: it may emit calls to tools this build
    /// does not have. Runnable, but the harness should surface it.
    CompatibleNewerMinor { checkpoint: Version, current: Version },
    /// Different major — refuse (§4.4).
    Incompatible { checkpoint: Version, current: Version },
    /// The checkpoint declared nothing parseable. Treated as incompatible: an
    /// undeclared schema is exactly the situation the contract exists to stop.
    Undeclared { declared: String },
}

impl Compatibility {
    /// Whether the harness should hand this checkpoint work at all.
    pub fn is_usable(&self) -> bool {
        !matches!(
            self,
            Compatibility::Incompatible { .. } | Compatibility::Undeclared { .. }
        )
    }
}

impl std::fmt::Display for Compatibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Compatibility::Exact => write!(f, "checkpoint matches the tool schema exactly"),
            Compatibility::CompatibleOlderMinor { checkpoint, current } => write!(
                f,
                "checkpoint targets schema {checkpoint}, harness ships {current}: compatible, \
                 but the checkpoint will not use tools added since {checkpoint}"
            ),
            Compatibility::CompatibleNewerMinor { checkpoint, current } => write!(
                f,
                "checkpoint targets schema {checkpoint}, harness ships {current}: compatible, \
                 but the checkpoint may call tools this build does not provide"
            ),
            Compatibility::Incompatible { checkpoint, current } => write!(
                f,
                "checkpoint targets schema {checkpoint}, harness ships {current}: incompatible \
                 major version — the toolset changed in a breaking way"
            ),
            Compatibility::Undeclared { declared } => write!(
                f,
                "checkpoint declared no usable schema version ({declared:?}); refusing"
            ),
        }
    }
}

/// Check a checkpoint's declared schema version against the embedded contract.
pub fn check_checkpoint(declared: &str) -> Compatibility {
    let current = schema().version();
    let Some(checkpoint) = Version::parse(declared) else {
        return Compatibility::Undeclared {
            declared: declared.to_string(),
        };
    };
    if checkpoint.major != current.major {
        return Compatibility::Incompatible {
            checkpoint,
            current,
        };
    }
    match checkpoint.minor.cmp(&current.minor) {
        std::cmp::Ordering::Equal => Compatibility::Exact,
        std::cmp::Ordering::Less => Compatibility::CompatibleOlderMinor {
            checkpoint,
            current,
        },
        std::cmp::Ordering::Greater => Compatibility::CompatibleNewerMinor {
            checkpoint,
            current,
        },
    }
}
