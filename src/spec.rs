//! Architecture specification language.
//!
//! A spec declaratively describes the *target* module structure for a
//! codebase: which items belong where, visibility rules, and allowed
//! dependency boundaries. The global planner consumes a parsed `Spec` and
//! the `Discovery` index to produce a migration plan.
//!
//! Supports JSON and YAML. Files are auto-detected by extension.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Top-level architecture specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// Optional schema version. Omitted legacy specs are treated as version 1.
    #[serde(default = "default_version")]
    pub version: u32,

    /// Named target modules (buckets). Order matters: first match wins.
    #[serde(default)]
    pub modules: Vec<ModuleSpec>,

    /// Dependency boundaries between modules.
    #[serde(default)]
    pub boundaries: BoundaryPolicy,

    /// Fallback behaviour for items that match no module pattern.
    #[serde(default)]
    pub defaults: DefaultsSpec,

    /// Minimum evidence required for boundary/rewrite decisions.
    #[serde(default)]
    pub confidence: ConfidencePolicy,

    /// Default target layout policy.
    #[serde(default)]
    pub layout: LayoutPolicy,

    /// Default visibility policy.
    #[serde(default)]
    pub visibility: VisibilityPolicy,

    /// Optional local-LLM advisor policy.
    #[serde(default)]
    pub llm: LlmPolicy,
}

/// A target module and the rules for populating it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleSpec {
    /// Module name (becomes a file stem or directory name).
    pub name: String,

    /// Matching rules. An item that satisfies *any* rule is assigned here.
    #[serde(default)]
    pub patterns: Vec<Pattern>,

    /// Visibility for items moved into this module.
    /// Values: `private`, `pub`, `pub(crate)`, `pub(super)`.
    #[serde(default = "default_visibility")]
    pub visibility: String,

    /// Optional output path template, relative to the crate root.
    /// Placeholder `{module}` is replaced with the module name.
    /// If omitted, the planner picks a conventional location.
    pub output: Option<String>,
}

/// A single matching rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Pattern {
    /// Shorthand: just a glob string, equivalent to `name_glob`.
    Glob(String),
    /// Full form with explicit type.
    Typed {
        #[serde(rename = "type")]
        kind: PatternKind,
        value: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    /// Match item name against a glob (`*Model`, `parse_*`).
    NameGlob,
    /// Match item name against a regex.
    NameRegex,
    /// Match the original file path against a glob.
    FileGlob,
    /// Match item kind (`fn`, `struct`, `trait`, `impl`, etc.).
    Kind,
}

/// A dependency boundary rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BoundaryPolicy {
    Legacy(Vec<BoundarySpec>),
    Explicit {
        #[serde(default)]
        default: BoundaryDefault,
        #[serde(default)]
        rules: Vec<BoundarySpec>,
    },
}

impl BoundaryPolicy {
    pub fn rules(&self) -> &[BoundarySpec] {
        match self {
            BoundaryPolicy::Legacy(rules) => rules,
            BoundaryPolicy::Explicit { rules, .. } => rules,
        }
    }

    pub fn default_mode(&self) -> BoundaryDefault {
        match self {
            BoundaryPolicy::Legacy(rules) if rules.is_empty() => BoundaryDefault::Allow,
            BoundaryPolicy::Legacy(_) => BoundaryDefault::Deny,
            BoundaryPolicy::Explicit { default, .. } => default.clone(),
        }
    }
}

impl Default for BoundaryPolicy {
    fn default() -> Self {
        BoundaryPolicy::Legacy(Vec::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryDefault {
    Allow,
    Deny,
    Warn,
}

impl Default for BoundaryDefault {
    fn default() -> Self {
        BoundaryDefault::Allow
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundarySpec {
    /// Source module.
    pub from: String,
    /// Modules that `from` is allowed to reference.
    #[serde(default)]
    pub may_depend_on: Vec<String>,
}

/// Fallback defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsSpec {
    /// Module name for unmatched items.
    #[serde(default = "default_unmatched_module")]
    pub unmatched_module: String,
    /// Visibility for unmatched items.
    #[serde(default = "default_visibility")]
    pub unmatched_visibility: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfidencePolicy {
    #[serde(default = "default_boundary_confidence")]
    pub min_for_boundary: String,
    #[serde(default = "default_rewrite_confidence")]
    pub min_for_rewrite: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutPolicy {
    #[serde(default = "default_layout")]
    pub default: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisibilityPolicy {
    #[serde(default = "default_visibility")]
    pub default: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmPolicy {
    #[serde(default)]
    pub allow_advice: bool,
}

// ---------------------------------------------------------------------------
// Defaults

fn default_version() -> u32 {
    1
}

fn default_unmatched_module() -> String {
    "misc".to_string()
}

fn default_visibility() -> String {
    "private".to_string()
}

fn default_boundary_confidence() -> String {
    "semantic".to_string()
}

fn default_rewrite_confidence() -> String {
    "exact".to_string()
}

fn default_layout() -> String {
    "file_per_module".to_string()
}

impl Default for DefaultsSpec {
    fn default() -> Self {
        Self {
            unmatched_module: default_unmatched_module(),
            unmatched_visibility: default_visibility(),
        }
    }
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            version: default_version(),
            modules: Vec::new(),
            boundaries: BoundaryPolicy::default(),
            defaults: DefaultsSpec::default(),
            confidence: ConfidencePolicy::default(),
            layout: LayoutPolicy::default(),
            visibility: VisibilityPolicy::default(),
            llm: LlmPolicy::default(),
        }
    }
}

impl Default for ConfidencePolicy {
    fn default() -> Self {
        Self {
            min_for_boundary: default_boundary_confidence(),
            min_for_rewrite: default_rewrite_confidence(),
        }
    }
}

impl Default for LayoutPolicy {
    fn default() -> Self {
        Self {
            default: default_layout(),
        }
    }
}

impl Default for VisibilityPolicy {
    fn default() -> Self {
        Self {
            default: default_visibility(),
        }
    }
}

impl Default for LlmPolicy {
    fn default() -> Self {
        Self {
            allow_advice: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing

/// Parse a spec from a file path. Supports `.json` and `.yaml`/`.yml`.
pub fn parse_spec(path: &Path) -> anyhow::Result<Spec> {
    let src = std::fs::read_to_string(path)?;
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let spec = match ext {
        "yaml" | "yml" => yaml_serde::from_str(&src)?,
        "json" => serde_json::from_str(&src)?,
        _ => {
            // Try YAML first (more permissive), then JSON.
            yaml_serde::from_str(&src)
                .or_else(|_| serde_json::from_str(&src))
                .map_err(|e| anyhow::anyhow!("failed to parse spec as YAML or JSON: {e}"))?
        }
    };
    Ok(spec)
}

// ---------------------------------------------------------------------------
// Validation

/// Result of validating a spec.
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate a spec for structural correctness.
pub fn validate(spec: &Spec) -> ValidationReport {
    let mut report = ValidationReport::default();

    // 1. Module names must be unique.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for m in &spec.modules {
        if !seen.insert(&m.name) {
            report
                .errors
                .push(format!("duplicate module name: {}", m.name));
        }
    }

    // 2. Boundary rules must reference known modules.
    let mut known: BTreeSet<&str> = spec.modules.iter().map(|m| m.name.as_str()).collect();
    known.insert(&spec.defaults.unmatched_module);

    for b in spec.boundaries.rules() {
        if !known.contains(b.from.as_str()) {
            report.warnings.push(format!(
                "boundary 'from' module '{}' is not defined in modules list",
                b.from
            ));
        }
        for dep in &b.may_depend_on {
            if !known.contains(dep.as_str()) {
                report.warnings.push(format!(
                    "boundary dependency '{}' (from '{}') is not defined in modules list",
                    dep, b.from
                ));
            }
        }
    }

    // 3. Boundary default must be valid through serde; referenced unknowns are
    // promoted to errors for explicit policies because they make enforcement
    // misleading.
    if matches!(spec.boundaries, BoundaryPolicy::Explicit { .. }) {
        for b in spec.boundaries.rules() {
            if !known.contains(b.from.as_str()) {
                report.errors.push(format!(
                    "boundary 'from' module '{}' is not defined in modules list",
                    b.from
                ));
            }
            for dep in &b.may_depend_on {
                if !known.contains(dep.as_str()) {
                    report.errors.push(format!(
                        "boundary dependency '{}' (from '{}') is not defined in modules list",
                        dep, b.from
                    ));
                }
            }
        }
    }

    // 4. Visibility values must be one of the allowed set.
    let allowed_vis = ["private", "pub", "pub(crate)", "pub(super)"];
    for m in &spec.modules {
        if !allowed_vis.contains(&m.visibility.as_str()) {
            report.errors.push(format!(
                "module '{}' has invalid visibility: '{}'",
                m.name, m.visibility
            ));
        }
    }
    if !allowed_vis.contains(&spec.defaults.unmatched_visibility.as_str()) {
        report.errors.push(format!(
            "defaults.unmatched_visibility has invalid value: '{}'",
            spec.defaults.unmatched_visibility
        ));
    }
    if !allowed_vis.contains(&spec.visibility.default.as_str()) {
        report.errors.push(format!(
            "visibility.default has invalid value: '{}'",
            spec.visibility.default
        ));
    }

    // 5. Policy values must be valid.
    let allowed_confidence = [
        "exact",
        "semantic",
        "scoped_candidate",
        "text_candidate",
        "ambiguous",
        "unresolved",
    ];
    if !allowed_confidence.contains(&spec.confidence.min_for_boundary.as_str()) {
        report.errors.push(format!(
            "confidence.min_for_boundary has invalid value: '{}'",
            spec.confidence.min_for_boundary
        ));
    }
    if !allowed_confidence.contains(&spec.confidence.min_for_rewrite.as_str()) {
        report.errors.push(format!(
            "confidence.min_for_rewrite has invalid value: '{}'",
            spec.confidence.min_for_rewrite
        ));
    }
    let allowed_layout = ["file_per_module", "preserve", "directory_module"];
    if !allowed_layout.contains(&spec.layout.default.as_str()) {
        report.errors.push(format!(
            "layout.default has invalid value: '{}'",
            spec.layout.default
        ));
    }

    // 6. Regex patterns must compile.
    for m in &spec.modules {
        for p in &m.patterns {
            if let Pattern::Typed {
                kind: PatternKind::NameRegex,
                value,
            } = p
            {
                if regex::Regex::new(value).is_err() {
                    report.errors.push(format!(
                        "module '{}' has invalid regex pattern: '{}'",
                        m.name, value
                    ));
                }
            }
        }
    }

    report
}

// ---------------------------------------------------------------------------
// Matching engine

/// Match an item against a spec and return the first matching module.
pub fn match_item<'a>(
    spec: &'a Spec,
    name: &'a str,
    kind: &'a str,
    file_path: &'a Path,
) -> Option<&'a ModuleSpec> {
    for m in &spec.modules {
        for p in &m.patterns {
            if pattern_matches(p, name, kind, file_path) {
                return Some(m);
            }
        }
    }
    None
}

fn pattern_matches(pat: &Pattern, name: &str, kind: &str, file_path: &Path) -> bool {
    match pat {
        Pattern::Glob(g) => glob_match(g, name),
        Pattern::Typed { kind: pk, value } => match pk {
            PatternKind::NameGlob => glob_match(value, name),
            PatternKind::NameRegex => regex_match(value, name),
            PatternKind::FileGlob => {
                let file_str = file_path.to_string_lossy();
                glob_match(value, &file_str)
            }
            PatternKind::Kind => kind.eq_ignore_ascii_case(value),
        },
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    // Simple glob: `*` matches any sequence, `?` matches one char.
    let regex_str = regex::escape(pattern)
        .replace(r"\*", ".*")
        .replace(r"\?", ".");
    regex::Regex::new(&format!("^{}$", regex_str))
        .map(|re| re.is_match(text))
        .unwrap_or(false)
}

fn regex_match(pattern: &str, text: &str) -> bool {
    regex::Regex::new(pattern)
        .map(|re| re.is_match(text))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> Spec {
        Spec {
            version: 1,
            modules: vec![
                ModuleSpec {
                    name: "domain".to_string(),
                    patterns: vec![
                        Pattern::Glob("*Model".to_string()),
                        Pattern::Typed {
                            kind: PatternKind::NameGlob,
                            value: "*Repository".to_string(),
                        },
                    ],
                    visibility: "pub(crate)".to_string(),
                    output: None,
                },
                ModuleSpec {
                    name: "application".to_string(),
                    patterns: vec![Pattern::Glob("*Service".to_string())],
                    visibility: "pub".to_string(),
                    output: Some("src/app/{module}.rs".to_string()),
                },
            ],
            boundaries: BoundaryPolicy::Legacy(vec![BoundarySpec {
                from: "application".to_string(),
                may_depend_on: vec!["domain".to_string()],
            }]),
            defaults: DefaultsSpec::default(),
            confidence: ConfidencePolicy::default(),
            layout: LayoutPolicy::default(),
            visibility: VisibilityPolicy::default(),
            llm: LlmPolicy::default(),
        }
    }

    #[test]
    fn glob_star_matches_suffix() {
        assert!(glob_match("*Model", "UserModel"));
        assert!(!glob_match("*Model", "UserService"));
    }

    #[test]
    fn glob_question_mark() {
        assert!(glob_match("?at", "cat"));
        assert!(!glob_match("?at", "chat"));
    }

    #[test]
    fn regex_pattern_works() {
        let pat = Pattern::Typed {
            kind: PatternKind::NameRegex,
            value: "^.*(Model|Repository)$".to_string(),
        };
        assert!(pattern_matches(
            &pat,
            "UserModel",
            "struct",
            Path::new("src/lib.rs")
        ));
        assert!(pattern_matches(
            &pat,
            "UserRepository",
            "struct",
            Path::new("src/lib.rs")
        ));
        assert!(!pattern_matches(
            &pat,
            "UserService",
            "struct",
            Path::new("src/lib.rs")
        ));
    }

    #[test]
    fn kind_pattern_matches() {
        let pat = Pattern::Typed {
            kind: PatternKind::Kind,
            value: "fn".to_string(),
        };
        assert!(pattern_matches(&pat, "foo", "fn", Path::new("src/lib.rs")));
        assert!(!pattern_matches(
            &pat,
            "foo",
            "struct",
            Path::new("src/lib.rs")
        ));
    }

    #[test]
    fn file_glob_matches() {
        let pat = Pattern::Typed {
            kind: PatternKind::FileGlob,
            value: "*/domain/*.rs".to_string(),
        };
        assert!(pattern_matches(
            &pat,
            "foo",
            "fn",
            Path::new("src/domain/user.rs")
        ));
        assert!(!pattern_matches(
            &pat,
            "foo",
            "fn",
            Path::new("src/app/user.rs")
        ));
    }

    #[test]
    fn match_item_selects_first_module() {
        let spec = sample_spec();
        let m = match_item(&spec, "UserModel", "struct", Path::new("src/lib.rs"));
        assert!(m.is_some());
        assert_eq!(m.unwrap().name, "domain");
    }

    #[test]
    fn match_item_returns_none_for_unknown() {
        let spec = sample_spec();
        let m = match_item(&spec, "Helper", "fn", Path::new("src/lib.rs"));
        assert!(m.is_none());
    }

    #[test]
    fn validation_catches_duplicate_module() {
        let mut spec = sample_spec();
        spec.modules.push(spec.modules[0].clone());
        let report = validate(&spec);
        assert!(!report.is_valid());
        assert!(report.errors.iter().any(|e| e.contains("duplicate")));
    }

    #[test]
    fn validation_catches_invalid_visibility() {
        let mut spec = sample_spec();
        spec.modules[0].visibility = "public".to_string();
        let report = validate(&spec);
        assert!(!report.is_valid());
        assert!(report.errors.iter().any(|e| e.contains("visibility")));
    }

    #[test]
    fn validation_catches_bad_regex() {
        let mut spec = sample_spec();
        spec.modules[0].patterns = vec![Pattern::Typed {
            kind: PatternKind::NameRegex,
            value: "[invalid".to_string(),
        }];
        let report = validate(&spec);
        assert!(!report.is_valid());
        assert!(report.errors.iter().any(|e| e.contains("regex")));
    }

    #[test]
    fn validation_warns_on_unknown_boundary_module() {
        let mut spec = sample_spec();
        let mut rules = spec.boundaries.rules().to_vec();
        rules.push(BoundarySpec {
            from: "unknown".to_string(),
            may_depend_on: vec![],
        });
        spec.boundaries = BoundaryPolicy::Legacy(rules);
        let report = validate(&spec);
        assert!(report.is_valid());
        assert!(report.warnings.iter().any(|w| w.contains("unknown")));
    }

    #[test]
    fn explicit_boundary_unknown_module_is_error() {
        let mut spec = sample_spec();
        spec.boundaries = BoundaryPolicy::Explicit {
            default: BoundaryDefault::Deny,
            rules: vec![BoundarySpec {
                from: "unknown".to_string(),
                may_depend_on: vec![],
            }],
        };
        let report = validate(&spec);
        assert!(!report.is_valid());
        assert!(report.errors.iter().any(|e| e.contains("unknown")));
    }

    #[test]
    fn validation_catches_invalid_policy_values() {
        let mut spec = sample_spec();
        spec.confidence.min_for_rewrite = "wishful".to_string();
        spec.layout.default = "maze".to_string();
        spec.visibility.default = "public".to_string();
        let report = validate(&spec);
        assert!(!report.is_valid());
        assert!(report.errors.iter().any(|e| e.contains("confidence")));
        assert!(report.errors.iter().any(|e| e.contains("layout")));
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("visibility.default"))
        );
    }

    #[test]
    fn parse_json_spec_round_trip() {
        let spec = sample_spec();
        let json = serde_json::to_string_pretty(&spec).unwrap();
        let parsed: Spec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.modules.len(), 2);
    }

    #[test]
    fn parse_yaml_spec_round_trip() {
        let spec = sample_spec();
        let yaml = yaml_serde::to_string(&spec).unwrap();
        let parsed: Spec = yaml_serde::from_str(&yaml).unwrap();
        assert_eq!(parsed.modules.len(), 2);
    }
}
