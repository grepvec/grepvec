//! Tree-Sitter Validator Module
//!
//! This module implements LAW #4: THE TREE-SITTER CONSTITUTIONAL CONSTRAINT
//!
//! Every file must be parseable by Tree-Sitter into a deterministic syntax tree.
//! If code cannot be parsed, it is rejected and must be refactored.

use crate::compliance::{
    ComplianceReport, FileReport, IllegalPattern, IllegalPatternType,
    LanguageCoverage, ParseResult, PatternLocation, PatternSeverity,
};
use std::path::{Path, PathBuf};

/// Supported languages for Tree-Sitter validation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    C,
}

impl Language {
    /// Detect language from file extension
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "rs" => Some(Language::Rust),
            "ts" | "tsx" => Some(Language::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "py" | "pyi" => Some(Language::Python),
            "go" => Some(Language::Go),
            "c" | "h" => Some(Language::C),
            _ => None,
        }
    }

    /// Get language name as string
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
            Language::Python => "python",
            Language::Go => "go",
            Language::C => "c",
        }
    }

    /// Get file extensions for this language
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["rs"],
            Language::TypeScript => &["ts", "tsx"],
            Language::JavaScript => &["js", "jsx", "mjs", "cjs"],
            Language::Python => &["py", "pyi"],
            Language::Go => &["go"],
            Language::C => &["c", "h"],
        }
    }
}

/// Configuration for the validator
#[derive(Debug, Clone)]
pub struct ValidatorConfig {
    /// Languages to validate
    pub languages: Vec<Language>,

    /// File patterns to ignore (e.g., "*.min.js", "vendor/*")
    pub ignore_patterns: Vec<String>,

    /// Maximum file size to parse (bytes)
    pub max_file_size: usize,

    /// Whether to detect illegal patterns
    pub detect_illegal_patterns: bool,

    /// Whether to extract node statistics
    pub extract_node_stats: bool,
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            languages: vec![
                Language::Rust,
                Language::TypeScript,
                Language::JavaScript,
                Language::Python,
                Language::Go,
                Language::C,
            ],
            ignore_patterns: vec![
                "*.min.js".to_string(),
                "*.min.css".to_string(),
                "**/node_modules/**".to_string(),
                "**/target/**".to_string(),
                "**/dist/**".to_string(),
                "**/build/**".to_string(),
                "**/.git/**".to_string(),
                "**/vendor/**".to_string(),
            ],
            max_file_size: 1024 * 1024, // 1MB
            detect_illegal_patterns: true,
            extract_node_stats: true,
        }
    }
}

/// The main validator struct
pub struct TreeSitterValidator {
    config: ValidatorConfig,
    // Note: Parsers are created on-demand per parse operation because
    // tree_sitter::Parser is not Send/Sync safe for sharing across threads
}

impl TreeSitterValidator {
    /// Create a new validator with default config
    pub fn new() -> Self {
        Self::with_config(ValidatorConfig::default())
    }

    /// Create a validator with custom config
    pub fn with_config(config: ValidatorConfig) -> Self {
        Self { config }
    }

    /// Validate an entire repository
    pub fn validate_repo(&self, repo_path: &Path) -> Result<ComplianceReport, ValidatorError> {
        let mut report = ComplianceReport::new(repo_path.to_path_buf());

        // Walk the directory tree
        let files = self.collect_files(repo_path)?;

        // Parse each file
        for file_path in files {
            let file_report = self.validate_file(&file_path, repo_path)?;

            // Collect illegal patterns
            for pattern_loc in &file_report.illegal_patterns {
                if let Some(pattern) = self.create_illegal_pattern(&file_path, pattern_loc) {
                    report.illegal_patterns.push(pattern);
                }
            }

            // Update language coverage
            let lang_name = file_report.language.clone();
            let coverage = report.languages.entry(lang_name.clone()).or_insert_with(|| {
                LanguageCoverage {
                    language: lang_name,
                    file_count: 0,
                    total_lines: 0,
                    parsed_successfully: 0,
                    illegal_patterns: 0,
                    compliance_percentage: 0.0,
                }
            });

            coverage.file_count += 1;
            coverage.total_lines += file_report.line_count;
            if matches!(file_report.parse_result, ParseResult::Success) {
                coverage.parsed_successfully += 1;
            }
            coverage.illegal_patterns += file_report.illegal_patterns.len();

            report.files.push(file_report);
        }

        // Calculate language compliance percentages
        for coverage in report.languages.values_mut() {
            if coverage.file_count > 0 {
                coverage.compliance_percentage =
                    (coverage.parsed_successfully as f64 / coverage.file_count as f64) * 100.0;
            }
        }

        // Calculate summary
        report.calculate_summary();

        Ok(report)
    }

    /// Validate a single file
    pub fn validate_file(
        &self,
        file_path: &Path,
        repo_root: &Path,
    ) -> Result<FileReport, ValidatorError> {
        let relative_path = file_path
            .strip_prefix(repo_root)
            .unwrap_or(file_path)
            .to_path_buf();

        let extension = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let language = Language::from_extension(extension);

        let language_str = language
            .map(|l| l.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Check file size
        let metadata = std::fs::metadata(file_path)
            .map_err(|e| ValidatorError::IoError(e.to_string()))?;

        if metadata.len() > self.config.max_file_size as u64 {
            return Ok(FileReport {
                path: relative_path,
                language: language_str,
                parse_result: ParseResult::Skipped {
                    reason: format!("File too large: {} bytes", metadata.len()),
                },
                line_count: 0,
                node_count: 0,
                illegal_patterns: vec![],
            });
        }

        // Read file content
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| ValidatorError::IoError(e.to_string()))?;

        let line_count = content.lines().count();

        // Skip if no parser available for this language
        let Some(lang) = language else {
            return Ok(FileReport {
                path: relative_path,
                language: language_str,
                parse_result: ParseResult::Skipped {
                    reason: "Unsupported language".to_string(),
                },
                line_count,
                node_count: 0,
                illegal_patterns: vec![],
            });
        };

        // Parse with Tree-Sitter
        let parse_result = self.parse_content(&content, lang);

        let (parse_result, node_count, illegal_patterns) = match parse_result {
            Ok((nodes, patterns)) => (ParseResult::Success, nodes, patterns),
            Err(e) => (
                ParseResult::Failed {
                    error: e.to_string(),
                },
                0,
                vec![],
            ),
        };

        Ok(FileReport {
            path: relative_path,
            language: language_str,
            parse_result,
            line_count,
            node_count,
            illegal_patterns,
        })
    }

    /// Collect all relevant files from a repository
    fn collect_files(&self, repo_path: &Path) -> Result<Vec<PathBuf>, ValidatorError> {
        let mut files = Vec::new();

        // Use walkdir to traverse (will be added as dependency)
        // For now, simple recursive implementation
        self.collect_files_recursive(repo_path, &mut files)?;

        Ok(files)
    }

    fn collect_files_recursive(
        &self,
        dir: &Path,
        files: &mut Vec<PathBuf>,
    ) -> Result<(), ValidatorError> {
        if !dir.is_dir() {
            return Ok(());
        }

        let entries = std::fs::read_dir(dir)
            .map_err(|e| ValidatorError::IoError(e.to_string()))?;

        for entry in entries {
            let entry = entry.map_err(|e| ValidatorError::IoError(e.to_string()))?;
            let path = entry.path();

            // Skip ignored patterns
            if self.should_ignore(&path) {
                continue;
            }

            if path.is_dir() {
                self.collect_files_recursive(&path, files)?;
            } else if path.is_file() {
                // Check if it's a supported file type
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if Language::from_extension(ext).is_some() {
                        files.push(path);
                    }
                }
            }
        }

        Ok(())
    }

    fn should_ignore(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();

        for pattern in &self.config.ignore_patterns {
            if pattern.contains("**") {
                // Extract the meaningful directory name between ** separators.
                // Handles: **/dirname/**, **/dirname, dirname/**
                let inner = pattern.replace("**", "");
                let inner = inner.trim_matches('/');

                if inner.is_empty() {
                    continue;
                }

                // Check if any path component matches the inner directory name
                for component in path.components() {
                    if let std::path::Component::Normal(c) = component {
                        if c.to_string_lossy() == inner {
                            return true;
                        }
                    }
                }
            } else if pattern.starts_with('*') {
                // Suffix match: *.min.js -> ends with .min.js
                let suffix = pattern.trim_start_matches('*');
                if path_str.ends_with(suffix) {
                    return true;
                }
            }
        }

        false
    }

    /// Get or create a parser for the given language
    /// Note: tree_sitter::Parser is not Send, so we create a new one each time
    fn get_parser(&self, language: Language) -> Result<tree_sitter::Parser, ValidatorError> {
        let mut parser = tree_sitter::Parser::new();

        let ts_language = match language {
            Language::Rust => tree_sitter_rust::language(),
            Language::TypeScript => tree_sitter_typescript::language_tsx(),
            Language::JavaScript => tree_sitter_typescript::language_tsx(),
            Language::Python => tree_sitter_python::language(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
        };

        parser
            .set_language(&ts_language)
            .map_err(|e| ValidatorError::ParseError(format!("Failed to set language: {}", e)))?;

        Ok(parser)
    }

    /// Count all nodes in the AST recursively
    fn count_nodes(node: tree_sitter::Node) -> usize {
        let mut count = 1; // Count this node
        let mut cursor = node.walk();

        for child in node.children(&mut cursor) {
            count += Self::count_nodes(child);
        }

        count
    }

    /// Parse content and detect illegal patterns
    /// Returns (node_count, illegal_patterns)
    fn parse_content(
        &self,
        content: &str,
        language: Language,
    ) -> Result<(usize, Vec<PatternLocation>), ValidatorError> {
        // Get a parser for this language
        let mut parser = self.get_parser(language)?;

        // Parse the content
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| ValidatorError::ParseError("Failed to parse content".to_string()))?;

        // Count AST nodes
        let node_count = if self.config.extract_node_stats {
            Self::count_nodes(tree.root_node())
        } else {
            0
        };

        // Detect illegal patterns (still using regex-based detection for now)
        // TODO: In the future, this could be enhanced to use AST-based pattern matching
        let illegal_patterns = if self.config.detect_illegal_patterns {
            self.detect_illegal_patterns_simple(content, language)
        } else {
            vec![]
        };

        Ok((node_count, illegal_patterns))
    }

    /// Simple regex-based illegal pattern detection
    /// This is a placeholder until proper Tree-Sitter AST analysis is implemented
    fn detect_illegal_patterns_simple(
        &self,
        content: &str,
        language: Language,
    ) -> Vec<PatternLocation> {
        let mut patterns = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            let detections = match language {
                Language::Python => self.detect_python_illegal_patterns(line),
                Language::JavaScript | Language::TypeScript => {
                    self.detect_js_illegal_patterns(line)
                }
                Language::Rust => self.detect_rust_illegal_patterns(line),
                Language::Go | Language::C => vec![], // No illegal pattern detection yet
            };

            for col in detections {
                patterns.push(PatternLocation {
                    file: PathBuf::new(), // Will be filled in by caller
                    line: line_num + 1,
                    column: col + 1,
                    end_line: None,
                    end_column: None,
                });
            }
        }

        patterns
    }

    fn detect_python_illegal_patterns(&self, line: &str) -> Vec<usize> {
        let mut positions = Vec::new();
        let trimmed = line.trim();

        // Skip comments
        if trimmed.starts_with('#') {
            return positions;
        }

        // eval()
        if let Some(pos) = line.find("eval(") {
            positions.push(pos);
        }

        // exec()
        if let Some(pos) = line.find("exec(") {
            positions.push(pos);
        }

        // getattr with variable
        if line.contains("getattr(") && !line.contains("getattr(self,") {
            if let Some(pos) = line.find("getattr(") {
                positions.push(pos);
            }
        }

        // __import__
        if let Some(pos) = line.find("__import__(") {
            positions.push(pos);
        }

        // importlib.import_module with variable
        if line.contains("import_module(") && line.contains("import_module(f\"") {
            if let Some(pos) = line.find("import_module(") {
                positions.push(pos);
            }
        }

        positions
    }

    fn detect_js_illegal_patterns(&self, line: &str) -> Vec<usize> {
        let mut positions = Vec::new();
        let trimmed = line.trim();

        // Skip comments
        if trimmed.starts_with("//") || trimmed.starts_with("/*") {
            return positions;
        }

        // eval()
        if let Some(pos) = line.find("eval(") {
            positions.push(pos);
        }

        // new Function()
        if let Some(pos) = line.find("new Function(") {
            positions.push(pos);
        }

        // Dynamic require with variable
        if line.contains("require(") && !line.contains("require(\"") && !line.contains("require('")
        {
            if let Some(pos) = line.find("require(") {
                positions.push(pos);
            }
        }

        // Dynamic import with variable (but not static import())
        if line.contains("import(") && !line.contains("import(\"") && !line.contains("import('") {
            if let Some(pos) = line.find("import(") {
                // Check it's not a static import
                if !line.contains("await import(\"") && !line.contains("await import('") {
                    positions.push(pos);
                }
            }
        }

        positions
    }

    fn detect_rust_illegal_patterns(&self, _line: &str) -> Vec<usize> {
        // Rust is generally Tree-Sitter compliant by design
        // Only check for proc macros that might generate unparseable code
        let positions = Vec::new();

        // Most Rust code is fine - proc macros expand to valid Rust
        // We might flag certain unsafe blocks for review, but they're still parseable

        positions
    }

    fn create_illegal_pattern(
        &self,
        file_path: &Path,
        location: &PatternLocation,
    ) -> Option<IllegalPattern> {
        // Read the line to get the code snippet
        let content = std::fs::read_to_string(file_path).ok()?;
        let lines: Vec<&str> = content.lines().collect();
        let line_content = lines.get(location.line.saturating_sub(1))?.to_string();

        // Determine pattern type from content
        let pattern_type = self.classify_pattern(&line_content);

        Some(IllegalPattern {
            pattern_type,
            location: PatternLocation {
                file: file_path.to_path_buf(),
                ..location.clone()
            },
            code_snippet: line_content.clone(),
            suggested_refactoring: self.suggest_refactoring(&line_content),
            severity: PatternSeverity::Medium,
        })
    }

    fn classify_pattern(&self, line: &str) -> IllegalPatternType {
        if line.contains("eval(") {
            IllegalPatternType::DynamicEval
        } else if line.contains("exec(") {
            IllegalPatternType::StringExecution
        } else if line.contains("getattr(") || line.contains("__getattribute__") {
            IllegalPatternType::Reflection
        } else if line.contains("__import__(")
            || line.contains("import_module(")
            || (line.contains("require(") && !line.contains("require(\""))
        {
            IllegalPatternType::DynamicImport
        } else if line.contains("new Function(") {
            IllegalPatternType::DynamicDefinition
        } else {
            IllegalPatternType::Other {
                description: "Unclassified illegal pattern".to_string(),
            }
        }
    }

    fn suggest_refactoring(&self, line: &str) -> Option<String> {
        if line.contains("eval(") {
            Some("Replace eval() with explicit function dispatch using a dictionary/map".to_string())
        } else if line.contains("exec(") {
            Some(
                "Replace exec() with pre-defined functions and explicit control flow".to_string(),
            )
        } else if line.contains("getattr(") {
            Some("Replace getattr() with explicit attribute access or a dispatch dictionary"
                .to_string())
        } else if line.contains("require(") && !line.contains("require(\"") {
            Some("Replace dynamic require() with static imports and a module registry".to_string())
        } else {
            None
        }
    }
}

impl Default for TreeSitterValidator {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors that can occur during validation
#[derive(Debug)]
pub enum ValidatorError {
    IoError(String),
    ParseError(String),
    ConfigError(String),
}

impl std::fmt::Display for ValidatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidatorError::IoError(e) => write!(f, "IO error: {}", e),
            ValidatorError::ParseError(e) => write!(f, "Parse error: {}", e),
            ValidatorError::ConfigError(e) => write!(f, "Config error: {}", e),
        }
    }
}

impl std::error::Error for ValidatorError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("tsx"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("js"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("txt"), None);
    }

    #[test]
    fn test_python_illegal_pattern_detection() {
        let validator = TreeSitterValidator::new();

        let patterns = validator.detect_python_illegal_patterns("result = eval(user_input)");
        assert_eq!(patterns.len(), 1);

        let patterns = validator.detect_python_illegal_patterns("exec(code_string)");
        assert_eq!(patterns.len(), 1);

        let patterns = validator.detect_python_illegal_patterns("x = 1 + 2");
        assert_eq!(patterns.len(), 0);

        // Comments should be ignored
        let patterns = validator.detect_python_illegal_patterns("# eval() is dangerous");
        assert_eq!(patterns.len(), 0);
    }

    #[test]
    fn test_js_illegal_pattern_detection() {
        let validator = TreeSitterValidator::new();

        let patterns = validator.detect_js_illegal_patterns("eval(userCode)");
        assert_eq!(patterns.len(), 1);

        let patterns = validator.detect_js_illegal_patterns("new Function('return ' + x)");
        assert_eq!(patterns.len(), 1);

        let patterns = validator.detect_js_illegal_patterns("const x = require(moduleName)");
        assert_eq!(patterns.len(), 1);

        let patterns = validator.detect_js_illegal_patterns("const x = require(\"lodash\")");
        assert_eq!(patterns.len(), 0);
    }

    #[test]
    fn test_should_ignore() {
        let validator = TreeSitterValidator::new();

        assert!(validator.should_ignore(Path::new("node_modules/lodash/index.js")));
        assert!(validator.should_ignore(Path::new("target/debug/build/something.rs")));
        assert!(validator.should_ignore(Path::new(".git/objects/abc123")));
        assert!(!validator.should_ignore(Path::new("src/main.rs")));
        assert!(!validator.should_ignore(Path::new("lib/utils.ts")));
    }

    #[test]
    fn test_tree_sitter_parsing_rust() {
        let validator = TreeSitterValidator::new();

        let rust_code = r#"
fn hello_world() {
    println!("Hello, world!");
}

struct Person {
    name: String,
    age: u32,
}
"#;

        let result = validator.parse_content(rust_code, Language::Rust);
        assert!(result.is_ok(), "Failed to parse valid Rust code");

        let (node_count, patterns) = result.unwrap();
        assert!(node_count > 0, "Should have counted AST nodes");
        assert_eq!(patterns.len(), 0, "No illegal patterns in this code");
    }

    #[test]
    fn test_tree_sitter_parsing_python() {
        let validator = TreeSitterValidator::new();

        let python_code = r#"
def greet(name):
    print(f"Hello, {name}!")

class Calculator:
    def add(self, a, b):
        return a + b
"#;

        let result = validator.parse_content(python_code, Language::Python);
        assert!(result.is_ok(), "Failed to parse valid Python code");

        let (node_count, _patterns) = result.unwrap();
        assert!(node_count > 0, "Should have counted AST nodes");
    }

    #[test]
    fn test_tree_sitter_parsing_typescript() {
        let validator = TreeSitterValidator::new();

        let ts_code = r#"
interface Person {
    name: string;
    age: number;
}

function greet(person: Person): void {
    console.log(`Hello, ${person.name}!`);
}
"#;

        let result = validator.parse_content(ts_code, Language::TypeScript);
        assert!(result.is_ok(), "Failed to parse valid TypeScript code");

        let (node_count, _patterns) = result.unwrap();
        assert!(node_count > 0, "Should have counted AST nodes");
    }

    #[test]
    fn test_tree_sitter_illegal_pattern_detection() {
        let validator = TreeSitterValidator::new();

        let python_with_eval = r#"
def dangerous_function(user_input):
    result = eval(user_input)
    return result
"#;

        let result = validator.parse_content(python_with_eval, Language::Python);
        assert!(result.is_ok(), "Should parse even with illegal patterns");

        let (node_count, patterns) = result.unwrap();
        assert!(node_count > 0, "Should have counted AST nodes");
        assert!(patterns.len() > 0, "Should detect eval() as illegal pattern");
    }
}
