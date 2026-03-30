---
name: DSL parser unwrap pattern — intentional and documented
description: The voom-dsl parser and compiler use unwrap() deliberately on grammar-guaranteed structures
type: project
---

`crates/voom-dsl/src/compiler.rs` has `#![allow(clippy::unwrap_used)]` with a doc comment explaining why: the unwrap() calls operate on grammar-guaranteed structures from the pest parser. The AST shape is validated before compilation. This is a deliberate decision, not oversight.

`crates/voom-dsl/src/parser.rs` has many `.unwrap()` calls on `inner.next()`, `find()`, etc. These are all on pest `Pair` structures whose grammar guarantees the child structure. For example, `run_if` grammar: `"run_if" ~ ident ~ "." ~ run_if_trigger` — unwrapping `.find(Rule::ident)` is guaranteed by the grammar.

Three `unreachable!()` calls in the DSL parser/compiler are also grammar-guaranteed:
- `parser.rs:866` — compare_op tokens
- `compiler.rs:102` — run_if_trigger restricted to "modified"|"completed" by grammar
- `compiler.rs:494` — track targets validated before compilation

**Risk:** LOW — these are post-parse invariant assertions. If the grammar changes and the code is not updated, you get a panic in tests, not in production. Tests exercise all grammar paths.

**How to apply:** Do not flag these as high-risk unwraps. Do note them as MEDIUM if grammar/code divergence is possible without test coverage.
