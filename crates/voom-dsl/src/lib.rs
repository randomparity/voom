//! VOOM DSL parser.
//!
//! Parses `.voom` policy files into a typed AST using a PEG grammar (pest).
//!
//! # Example
//!
//! ```
//! use voom_dsl::parse_policy;
//!
//! let input = r#"policy "example" {
//!     phase init {
//!         container mkv
//!     }
//! }"#;
//!
//! let ast = parse_policy(input).unwrap();
//! assert_eq!(ast.name, "example");
//! ```

pub mod ast;
pub mod errors;
pub mod parser;

pub use ast::*;
pub use errors::DslError;
pub use parser::parse_policy;
