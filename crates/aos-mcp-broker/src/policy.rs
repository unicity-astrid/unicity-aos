//! Per-principal policy decision point (PDP) for supervised `claude -p`
//! tool calls — the binding argument-level gate.
//!
//! # Two planes, one rule engine
//!
//! Every `mcp__aos__*` tool the supervised Claude invokes funnels through
//! [`crate::broker::handle_mcp_call`] before
//! [`crate::execute::dispatch_with_approval`] fans it out on the bus. That
//! is the ONE capsule-space point that holds the parsed `(tool_name,
//! arguments)` and can refuse to dispatch, so for that plane the PDP is
//! evaluated there UNCONDITIONALLY and IN-PROCESS: the broker chokepoint
//! cannot be routed around — the tool literally cannot execute without
//! passing it.
//!
//! Claude's NATIVE tools (`Bash`, `Write`, …) reach no such chokepoint —
//! they execute inside the `claude` process. The only per-call lever for
//! them is a PreToolUse `type:"mcp_tool"` hook, which calls back into this
//! SAME [`evaluate`] via the reserved [`crate::broker::PRETOOLUSE_GATE_TOOL`].
//! That path is honestly ADVISORY: the hook is read from a settings tier a
//! capable session can edit, the gate call is one Claude could skip, and the
//! platform fails open. So it is defence-in-depth on top of the host sandbox
//! and the binding `--disallowedTools` deny-list — never a substitute for
//! the in-process gate the `mcp__aos__*` plane enjoys. One operator rule
//! set, two enforcement strengths.
//!
//! # What it adds over the host capability PEP (non-redundancy)
//!
//! The kernel enforces CAPABILITIES at execution time
//! (`request_approval`): "may this principal's tool make this
//! capability-gated host call at all". That is coarse and ARGUMENT-BLIND
//! — a granted `fs_write` scope approves every path inside it; a granted
//! `host_process` cap approves every argv. This PDP adds the
//! INTRA-capability, argument-level layer the capability model cannot
//! express. It only ever NARROWS: the default is [`Decision::Allow`], so a
//! PDP that finds no matching rule (or fails to load) degrades to the
//! existing capability enforcement — never to "anything goes".
//!
//! # Honest limits (do NOT oversell this)
//!
//! * DENY rules over free-form strings (a shell command, a VFS path) are
//!   BEST-EFFORT, not a guarantee. They match the RAW argument value; the
//!   kernel resolves `home://` host-side, so a path deny on the
//!   pre-resolution literal is evadable (`home://./x`, `home://a/../x`),
//!   and a command deny is evadable by quoting / encoding. The ROBUST
//!   shapes are ALLOWLIST-style (`eq` / `prefix` against a closed set,
//!   e.g. an egress-host allowlist), not denylists.
//! * The reachable surface is the `mcp__aos__*` capsule tools Claude is
//!   allowed; the built-in shell/file tools are already removed by
//!   `REQUIRED_DENIES`, so a `rm -rf` rule guards a tool that may not even
//!   exist in the surface.
//! * The rule `reason` surfaced back to Claude is the operator's static
//!   `id`, NEVER a reflected argument substring — reflecting attacker-
//!   influenced args into the model's context is an injection vector.
//! * Matchers are linear (`eq`/`prefix`/`contains`/`glob`) — NO regex —
//!   so an attacker-influenced argument cannot trigger ReDoS on the
//!   tool-call hot path.

use astrid_sdk::prelude::*;
use serde::Deserialize;
use serde_json::Value;

/// Manifest `[env]` key holding the per-principal policy rule set (a JSON
/// array of [`Rule`]). Read via [`astrid_sdk::env::var_opt`] at decision
/// time, so it resolves the invoking principal's overlay — per-principal
/// rules with no cross-capsule KV write.
const POLICY_RULES_ENV: &str = "policy_rules";

/// Cap on the number of rules accepted from the `[env]` value. A fat-
/// fingered or hostile rule blob must not be able to make the
/// per-tool-call evaluation loop unbounded.
const MAX_RULES: usize = 256;

/// Cap on a single matcher's `value` / a rule's `tool` glob length. Bounds
/// the linear matcher cost and rejects kilobyte patterns.
const MAX_PATTERN_LEN: usize = 512;

/// Cap on a JSON-pointer length. Pointers index into the tool arguments;
/// a pathological pointer is rejected at load.
const MAX_POINTER_LEN: usize = 256;

/// What a matched rule does. v1 is deny/allow only — `ask` is deliberately
/// omitted because surfacing an interactive prompt needs an elicitation
/// primitive the broker cannot call synchronously here; deny + a clear
/// reason is the v1 fail-secure verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Effect {
    /// Refuse the tool call — the broker replies `isError` and never
    /// dispatches.
    Deny,
    /// Permit the tool call (an explicit allow short-circuits later deny
    /// rules — first-match-wins).
    Allow,
}

/// One argument predicate. ALL of a rule's matchers must hold (AND) for
/// the rule to fire; an empty matcher list makes the rule purely
/// tool-name scoped.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct ArgMatcher {
    /// RFC-6901 JSON pointer into the tool `arguments` object (e.g.
    /// `/path`, `/command`, `/url`).
    pub pointer: String,
    /// The comparison applied to the value at `pointer`.
    pub op: MatchOp,
    /// The operand. For `glob`, `*` matches any run and `?` any single
    /// character.
    pub value: String,
}

/// Argument comparison operators. All linear / backtrack-free — no regex,
/// so an attacker-influenced argument cannot induce ReDoS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MatchOp {
    /// Exact string equality.
    Eq,
    /// The value starts with the operand.
    Prefix,
    /// The value contains the operand as a substring.
    Contains,
    /// `*` / `?` wildcard match (linear two-pointer, no backtracking).
    Glob,
}

/// One declarative rule. Operator-authored; never agent-authored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Rule {
    /// Stable operator-chosen id. Surfaced to Claude as the deny reason —
    /// it MUST NOT embed argument values (injection).
    pub id: String,
    /// Effect when this rule fires.
    pub effect: Effect,
    /// Glob over the RAW tool name (no `mcp__aos__` prefix — the broker
    /// receives raw names). `*` matches any run, `?` any character.
    pub tool: String,
    /// Argument predicates; ALL must hold. Empty = tool-scoped rule.
    #[serde(default)]
    pub when: Vec<ArgMatcher>,
}

/// The PDP verdict for one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Permit — the broker proceeds to dispatch (capability enforcement
    /// still applies at execution time).
    Allow,
    /// Refuse — the broker replies `isError` with `reason` (the rule id)
    /// and never dispatches.
    Deny {
        /// The matched rule's `id`. Sanitized by construction (operator
        /// string, no reflected args).
        reason: String,
    },
}

/// Evaluate `rules` against one tool call. First-match-wins over the
/// ordered list; the default is [`Decision::Allow`] so the PDP only ever
/// narrows the surface the capability PEP already guards.
///
/// Pure and total: no host calls, no panics, no allocation beyond the
/// returned reason string. Fully unit-testable without a live bus.
pub(crate) fn evaluate(rules: &[Rule], tool_name: &str, arguments: &Value) -> Decision {
    for rule in rules {
        if !glob_match(&rule.tool, tool_name) {
            continue;
        }
        if rule.when.iter().all(|m| matcher_holds(m, arguments)) {
            return match rule.effect {
                Effect::Deny => Decision::Deny {
                    reason: rule.id.clone(),
                },
                Effect::Allow => Decision::Allow,
            };
        }
    }
    Decision::Allow
}

/// True when the matcher's operator relates the value at its pointer to
/// its operand. A pointer that does not resolve, or resolves to a
/// non-string scalar, is rendered to a string first (numbers/bools) or
/// fails to match (objects/arrays/null) — so a rule can only ever ACT on
/// a value it can actually see, and a missing argument never accidentally
/// satisfies a deny.
fn matcher_holds(matcher: &ArgMatcher, arguments: &Value) -> bool {
    let Some(target) = arguments.pointer(&matcher.pointer) else {
        return false;
    };
    let value = match target {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        // Null / array / object have no meaningful string form to match
        // against here; treat as "no match" rather than stringifying so a
        // deny can't fire on an unintended shape.
        _ => return false,
    };
    match matcher.op {
        MatchOp::Eq => value == matcher.value,
        MatchOp::Prefix => value.starts_with(&matcher.value),
        MatchOp::Contains => value.contains(&matcher.value),
        MatchOp::Glob => glob_match(&matcher.value, &value),
    }
}

/// Linear `*`/`?` glob match (classic two-pointer wildcard algorithm).
///
/// `*` matches any run (including empty), `?` matches exactly one
/// character. There is no alternation or repetition operator, so the
/// worst case is O(pattern × text) with a single backtrack pointer — it
/// cannot blow up the way a backtracking regex can.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    // Backtrack anchors: the last `*` position and the text index to
    // resume from when a tentative match fails.
    let (mut star, mut resume): (Option<usize>, usize) = (None, 0);

    while t < txt.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == txt[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            resume = t;
            p += 1;
        } else if let Some(sp) = star {
            // Mismatch under a prior `*` — let the `*` absorb one more
            // character and retry.
            p = sp + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    // Trailing `*`s match the empty remainder.
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

/// Load the invoking principal's rule set from the `policy_rules` `[env]`
/// value. Returns an EMPTY rule set (→ default allow → capability PEP is
/// the live boundary) on any of: unset/empty value, host read error,
/// JSON parse error, or a cap/shape violation — and emits a LOUD audit on
/// the failure paths so an operator monitoring `astrid.v1.audit.policy_*`
/// sees that policy is not in force.
///
/// This degrades to the capability PEP rather than failing CLOSED
/// (deny-all) on a config error, so a malformed rule blob or a transient
/// KV hiccup cannot brick every session. A deployment that wants strict
/// fail-closed on config error is a future hardening knob; the tradeoff
/// is recorded here deliberately.
pub(crate) fn load_rules() -> Vec<Rule> {
    let raw = match env::var_opt(POLICY_RULES_ENV) {
        Ok(Some(s)) if !s.trim().is_empty() => s,
        Ok(_) => return Vec::new(),
        Err(e) => {
            audit_load_failure("env_read_error");
            log::warn(format!(
                "{}: policy_rules env read failed: {e:?}",
                crate::profile::log_tag()
            ));
            return Vec::new();
        }
    };

    let parsed: Vec<Rule> = match serde_json::from_str(&raw) {
        Ok(rules) => rules,
        Err(e) => {
            audit_load_failure("parse_error");
            log::warn(format!(
                "{}: policy_rules failed to parse: {e}",
                crate::profile::log_tag()
            ));
            return Vec::new();
        }
    };

    if let Err(reason) = validate(&parsed) {
        audit_load_failure(reason);
        log::warn(format!(
            "{}: policy_rules rejected ({reason}); policy NOT in force",
            crate::profile::log_tag()
        ));
        return Vec::new();
    }
    parsed
}

/// Enforce the load-time caps. A violation rejects the WHOLE set (policy
/// degrades to the capability PEP + loud audit) rather than partially
/// applying a malformed blob — a half-applied rule set is harder to reason
/// about than none.
fn validate(rules: &[Rule]) -> Result<(), &'static str> {
    if rules.len() > MAX_RULES {
        return Err("too_many_rules");
    }
    for rule in rules {
        if rule.id.is_empty() || rule.id.len() > MAX_PATTERN_LEN {
            return Err("bad_rule_id");
        }
        if rule.tool.is_empty() || rule.tool.len() > MAX_PATTERN_LEN {
            return Err("bad_tool_glob");
        }
        for matcher in &rule.when {
            if matcher.pointer.is_empty() || matcher.pointer.len() > MAX_POINTER_LEN {
                return Err("bad_pointer");
            }
            // A JSON pointer is either empty (whole doc) or starts with
            // `/`; we already reject empty above, so require the `/`.
            if !matcher.pointer.starts_with('/') {
                return Err("bad_pointer");
            }
            if matcher.value.len() > MAX_PATTERN_LEN {
                return Err("oversize_matcher_value");
            }
        }
    }
    Ok(())
}

/// Audit a policy-load failure on `astrid.v1.audit.policy_load_failed`.
/// Best-effort — the bus failing here must not itself wedge the tool
/// path; the load already degraded to "no policy".
fn audit_load_failure(reason: &str) {
    let _ = ipc::publish_json(
        &crate::profile::audit_topic("policy_load_failed"),
        &serde_json::json!({ "reason": reason }),
    );
}

#[cfg(test)]
mod tests {
    fn install_test_profile() {
        crate::profile::install_aos();
    }

    use super::*;
    use serde_json::json;

    fn deny(id: &str, tool: &str, when: Vec<ArgMatcher>) -> Rule {
        Rule {
            id: id.into(),
            effect: Effect::Deny,
            tool: tool.into(),
            when,
        }
    }
    fn m(pointer: &str, op: MatchOp, value: &str) -> ArgMatcher {
        ArgMatcher {
            pointer: pointer.into(),
            op,
            value: value.into(),
        }
    }

    #[test]
    fn empty_rules_default_allow() {
        install_test_profile();
        assert_eq!(evaluate(&[], "fs_read", &json!({})), Decision::Allow);
    }

    #[test]
    fn tool_scoped_deny_fires_without_arg_matchers() {
        install_test_profile();
        let rules = vec![deny("no-http", "http_*", vec![])];
        assert_eq!(
            evaluate(&rules, "http_fetch", &json!({"url": "x"})),
            Decision::Deny {
                reason: "no-http".into()
            }
        );
        // A different tool is unaffected.
        assert_eq!(evaluate(&rules, "fs_read", &json!({})), Decision::Allow);
    }

    #[test]
    fn arg_matchers_are_anded() {
        install_test_profile();
        let rules = vec![deny(
            "scoped",
            "fs_write",
            vec![
                m("/path", MatchOp::Prefix, "home://.ssh"),
                m("/mode", MatchOp::Eq, "overwrite"),
            ],
        )];
        // Both hold → deny.
        assert_eq!(
            evaluate(
                &rules,
                "fs_write",
                &json!({"path": "home://.ssh/authorized_keys", "mode": "overwrite"})
            ),
            Decision::Deny {
                reason: "scoped".into()
            }
        );
        // Only one holds → allow (AND not satisfied).
        assert_eq!(
            evaluate(
                &rules,
                "fs_write",
                &json!({"path": "home://.ssh/authorized_keys", "mode": "append"})
            ),
            Decision::Allow
        );
    }

    #[test]
    fn first_match_wins_allow_short_circuits_later_deny() {
        install_test_profile();
        let rules = vec![
            Rule {
                id: "allow-readme".into(),
                effect: Effect::Allow,
                tool: "fs_write".into(),
                when: vec![m("/path", MatchOp::Eq, "home://README.md")],
            },
            deny("deny-all-writes", "fs_write", vec![]),
        ];
        // The earlier allow wins for the README.
        assert_eq!(
            evaluate(&rules, "fs_write", &json!({"path": "home://README.md"})),
            Decision::Allow
        );
        // Anything else falls to the deny.
        assert_eq!(
            evaluate(&rules, "fs_write", &json!({"path": "home://other"})),
            Decision::Deny {
                reason: "deny-all-writes".into()
            }
        );
    }

    #[test]
    fn missing_argument_never_satisfies_a_deny() {
        install_test_profile();
        let rules = vec![deny(
            "x",
            "fs_write",
            vec![m("/path", MatchOp::Contains, "secret")],
        )];
        // No `/path` in the args → matcher fails → allow.
        assert_eq!(
            evaluate(&rules, "fs_write", &json!({"other": "secret"})),
            Decision::Allow
        );
    }

    #[test]
    fn reason_is_rule_id_never_reflected_arguments() {
        install_test_profile();
        let rules = vec![deny(
            "denied-by-policy",
            "*",
            vec![m("/command", MatchOp::Contains, "rm -rf")],
        )];
        let d = evaluate(
            &rules,
            "shell",
            &json!({"command": "rm -rf / # attacker-controlled"}),
        );
        // The reason is the static id, NOT the attacker's command string.
        assert_eq!(
            d,
            Decision::Deny {
                reason: "denied-by-policy".into()
            }
        );
    }

    #[test]
    fn glob_matches_runs_and_single_chars() {
        install_test_profile();
        assert!(glob_match("mcp__aos__*", "mcp__aos__fs_read"));
        assert!(glob_match("fs_*", "fs_write"));
        assert!(glob_match("a?c", "abc"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*.md", "home://x/y/readme.md"));
        assert!(!glob_match("fs_*", "http_fetch"));
        assert!(!glob_match("a?c", "ac"));
        assert!(!glob_match("abc", "abcd"));
        // Pathological many-star patterns terminate (no backtracking
        // blowup): the first has no trailing `b` to match, the second does.
        assert!(!glob_match("a*a*a*a*a*b", "aaaaaaaaaaaaaaaaaaaa"));
        assert!(glob_match("a*a*a*a*a*b", "aaaaaaaaaaaaaaaaaaab"));
        assert!(glob_match("*a*a*a*", "xxaxxaxxax"));
    }

    #[test]
    fn numbers_and_bools_stringify_for_matching() {
        install_test_profile();
        let rules = vec![deny("n", "tool", vec![m("/n", MatchOp::Eq, "5")])];
        assert_eq!(
            evaluate(&rules, "tool", &json!({"n": 5})),
            Decision::Deny { reason: "n".into() }
        );
    }

    #[test]
    fn validate_enforces_caps() {
        install_test_profile();
        // A pointer not starting with `/` is rejected.
        let bad_ptr = vec![deny("x", "t", vec![m("path", MatchOp::Eq, "v")])];
        assert!(validate(&bad_ptr).is_err());
        // Empty id rejected.
        let empty_id = vec![deny("", "t", vec![])];
        assert!(validate(&empty_id).is_err());
        // Oversize value rejected.
        let big = vec![deny(
            "x",
            "t",
            vec![m("/p", MatchOp::Eq, &"a".repeat(MAX_PATTERN_LEN + 1))],
        )];
        assert!(validate(&big).is_err());
        // A well-formed rule passes.
        let ok = vec![deny("ok", "fs_*", vec![m("/path", MatchOp::Prefix, "x")])];
        assert!(validate(&ok).is_ok());
    }

    #[test]
    fn rules_deserialize_from_env_json_shape() {
        install_test_profile();
        let raw = r#"[
            {"id":"no-ssh-write","effect":"deny","tool":"fs_write",
             "when":[{"pointer":"/path","op":"contains","value":".ssh/"}]},
            {"id":"allow-fetch","effect":"allow","tool":"http_fetch","when":[]}
        ]"#;
        let rules: Vec<Rule> = serde_json::from_str(raw).unwrap();
        assert_eq!(rules.len(), 2);
        assert!(validate(&rules).is_ok());
        assert_eq!(rules[0].effect, Effect::Deny);
        assert_eq!(rules[0].when[0].op, MatchOp::Contains);
        assert_eq!(rules[1].effect, Effect::Allow);
    }
}
