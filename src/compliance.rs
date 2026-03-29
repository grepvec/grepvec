//! Compliance Report Module
//!
//! Data structures for Tree-Sitter validation experiments.
//! These types capture the results of running the Four Laws validation
//! against a codebase.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Overall compliance report for a codebase
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceReport {
    /// Path to the analyzed repository
    pub repo_path: PathBuf,

    /// Timestamp of analysis
    pub analyzed_at: String,

    /// Summary statistics
    pub summary: ComplianceSummary,

    /// Per-file results
    pub files: Vec<FileReport>,

    /// Illegal patterns detected (LAW #4 violations)
    pub illegal_patterns: Vec<IllegalPattern>,

    /// Node extraction results (LAW #2 validation)
    pub node_statistics: NodeStatistics,

    /// Languages detected and their coverage
    pub languages: HashMap<String, LanguageCoverage>,
}

/// High-level summary of compliance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceSummary {
    /// Total files scanned
    pub total_files: usize,

    /// Files successfully parsed by Tree-Sitter
    pub parsed_successfully: usize,

    /// Files that failed to parse
    pub parse_failures: usize,

    /// Compliance percentage (parsed / total * 100)
    pub compliance_percentage: f64,

    /// Total AST nodes analyzed across all files
    pub total_nodes_analyzed: usize,

    /// Number of legal (compliant) patterns
    pub legal_pattern_count: usize,

    /// Number of illegal patterns detected
    pub illegal_pattern_count: usize,

    /// Pattern compliance percentage (legal / total * 100)
    pub pattern_compliance_percentage: f64,

    /// Estimated refactoring effort
    pub refactoring_effort: RefactoringEffort,

    /// Overall verdict
    pub verdict: ComplianceVerdict,
}

/// Refactoring effort estimate
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RefactoringEffort {
    /// < 10 illegal patterns, no structural issues
    Low,
    /// 10-50 illegal patterns, some structural issues
    Medium,
    /// 50-200 illegal patterns, significant structural issues
    High,
    /// > 200 illegal patterns or fundamental incompatibilities
    Extensive,
}

/// Overall compliance verdict
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ComplianceVerdict {
    /// >95% compliant, ready for grepvec
    FullyCompliant,
    /// 80-95% compliant, minor refactoring needed
    MostlyCompliant,
    /// 50-80% compliant, moderate refactoring needed
    PartiallyCompliant,
    /// <50% compliant, significant refactoring needed
    NonCompliant,
}

/// Report for a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReport {
    /// Relative path from repo root
    pub path: PathBuf,

    /// Detected language
    pub language: String,

    /// Parse result
    pub parse_result: ParseResult,

    /// Line count
    pub line_count: usize,

    /// Node count extracted
    pub node_count: usize,

    /// Illegal patterns in this file
    pub illegal_patterns: Vec<PatternLocation>,
}

/// Result of parsing a file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParseResult {
    /// Successfully parsed
    Success,
    /// Parse failed with error
    Failed { error: String },
    /// Skipped (binary, too large, etc.)
    Skipped { reason: String },
}

/// An illegal pattern detected (violates LAW #4)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IllegalPattern {
    /// Type of illegal pattern
    pub pattern_type: IllegalPatternType,

    /// File location
    pub location: PatternLocation,

    /// The offending code snippet
    pub code_snippet: String,

    /// Suggested refactoring (if available)
    pub suggested_refactoring: Option<String>,

    /// Severity level
    pub severity: PatternSeverity,
}

/// Types of illegal patterns (LAW #4 violations)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum IllegalPatternType {
    /// eval() or similar dynamic code execution
    DynamicEval,

    /// Reflection-based access (getattr, __getattribute__, etc.)
    Reflection,

    /// Dynamic imports (importlib, require with variables)
    DynamicImport,

    /// Computed property access that can't be statically resolved
    ComputedPropertyAccess,

    /// Metaprogramming constructs (macros that generate unparseable code)
    Metaprogramming,

    /// exec() or similar string-to-code execution
    StringExecution,

    /// Dynamic function/class creation
    DynamicDefinition,

    /// Other unclassified illegal pattern
    Other { description: String },
}

/// Location of a pattern in source code
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternLocation {
    /// File path (relative to repo root)
    pub file: PathBuf,

    /// Line number (1-indexed)
    pub line: usize,

    /// Column number (1-indexed)
    pub column: usize,

    /// End line (for multi-line patterns)
    pub end_line: Option<usize>,

    /// End column
    pub end_column: Option<usize>,
}

/// Severity of an illegal pattern
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum PatternSeverity {
    /// Can be auto-refactored
    Low,
    /// Requires manual review but straightforward
    Medium,
    /// Complex refactoring needed
    High,
    /// Fundamental architectural issue
    Critical,
}

/// Statistics about extracted nodes (LAW #2 validation)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatistics {
    /// Total nodes extracted
    pub total_nodes: usize,

    /// Nodes by type
    pub by_type: HashMap<String, usize>,

    /// Edges extracted (imports, calls, type refs)
    pub total_edges: usize,

    /// Edges by type
    pub edges_by_type: HashMap<String, usize>,

    /// Lines of code represented as nodes
    pub loc_covered: usize,

    /// Lines of code NOT represented (orphan code)
    pub loc_orphaned: usize,

    /// Coverage percentage
    pub coverage_percentage: f64,
}

/// Language-specific coverage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageCoverage {
    /// Language name (rust, typescript, python, etc.)
    pub language: String,

    /// File count
    pub file_count: usize,

    /// Total lines
    pub total_lines: usize,

    /// Parse success count
    pub parsed_successfully: usize,

    /// Illegal pattern count
    pub illegal_patterns: usize,

    /// Language-specific compliance percentage
    pub compliance_percentage: f64,
}

impl ComplianceReport {
    /// Create a new empty report
    pub fn new(repo_path: PathBuf) -> Self {
        Self {
            repo_path,
            analyzed_at: chrono::Utc::now().to_rfc3339(),
            summary: ComplianceSummary::default(),
            files: Vec::new(),
            illegal_patterns: Vec::new(),
            node_statistics: NodeStatistics::default(),
            languages: HashMap::new(),
        }
    }

    /// Calculate summary statistics from file reports
    pub fn calculate_summary(&mut self) {
        let total = self.files.len();
        let parsed = self.files.iter()
            .filter(|f| matches!(f.parse_result, ParseResult::Success))
            .count();
        let failed = self.files.iter()
            .filter(|f| matches!(f.parse_result, ParseResult::Failed { .. }))
            .count();

        self.summary.total_files = total;
        self.summary.parsed_successfully = parsed;
        self.summary.parse_failures = failed;
        self.summary.compliance_percentage = if total > 0 {
            (parsed as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        // Calculate total nodes analyzed across all files
        let total_nodes: usize = self.files.iter().map(|f| f.node_count).sum();
        self.summary.total_nodes_analyzed = total_nodes;

        // Illegal patterns count
        self.summary.illegal_pattern_count = self.illegal_patterns.len();

        // Legal patterns = total nodes - illegal patterns
        // (This is a simplification; in reality each illegal pattern is one node)
        self.summary.legal_pattern_count = total_nodes.saturating_sub(self.illegal_patterns.len());

        // Pattern compliance percentage
        self.summary.pattern_compliance_percentage = if total_nodes > 0 {
            (self.summary.legal_pattern_count as f64 / total_nodes as f64) * 100.0
        } else {
            100.0
        };

        self.summary.refactoring_effort = self.estimate_refactoring_effort();
        self.summary.verdict = self.determine_verdict();
    }

    fn estimate_refactoring_effort(&self) -> RefactoringEffort {
        let count = self.illegal_patterns.len();
        let critical_count = self.illegal_patterns.iter()
            .filter(|p| p.severity == PatternSeverity::Critical)
            .count();

        if critical_count > 10 || count > 200 {
            RefactoringEffort::Extensive
        } else if count > 50 {
            RefactoringEffort::High
        } else if count > 10 {
            RefactoringEffort::Medium
        } else {
            RefactoringEffort::Low
        }
    }

    fn determine_verdict(&self) -> ComplianceVerdict {
        let pct = self.summary.compliance_percentage;
        if pct >= 95.0 && self.summary.illegal_pattern_count < 5 {
            ComplianceVerdict::FullyCompliant
        } else if pct >= 80.0 {
            ComplianceVerdict::MostlyCompliant
        } else if pct >= 50.0 {
            ComplianceVerdict::PartiallyCompliant
        } else {
            ComplianceVerdict::NonCompliant
        }
    }

    /// Generate human-readable summary
    pub fn to_human_readable(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("═══════════════════════════════════════════════════════════════\n"));
        output.push_str(&format!("  GREPVEC COMPLIANCE REPORT\n"));
        output.push_str(&format!("  Repository: {}\n", self.repo_path.display()));
        output.push_str(&format!("  Analyzed: {}\n", self.analyzed_at));
        output.push_str(&format!("═══════════════════════════════════════════════════════════════\n\n"));

        output.push_str(&format!("SUMMARY\n"));
        output.push_str(&format!("───────────────────────────────────────────────────────────────\n"));
        output.push_str(&format!("  Files scanned:       {}\n", self.summary.total_files));
        output.push_str(&format!("  Parsed successfully: {} ({:.1}%)\n",
            self.summary.parsed_successfully,
            self.summary.compliance_percentage));
        output.push_str(&format!("  Parse failures:      {}\n", self.summary.parse_failures));
        output.push_str(&format!("───────────────────────────────────────────────────────────────\n"));
        output.push_str(&format!("  AST nodes analyzed:  {}\n", self.summary.total_nodes_analyzed));
        output.push_str(&format!("  Legal patterns:      {} ({:.4}%)\n",
            self.summary.legal_pattern_count,
            self.summary.pattern_compliance_percentage));
        output.push_str(&format!("  Illegal patterns:    {} ({:.4}%)\n",
            self.summary.illegal_pattern_count,
            100.0 - self.summary.pattern_compliance_percentage));
        output.push_str(&format!("───────────────────────────────────────────────────────────────\n"));
        output.push_str(&format!("  Refactoring effort:  {:?}\n", self.summary.refactoring_effort));
        output.push_str(&format!("  Verdict:             {:?}\n\n", self.summary.verdict));

        if !self.languages.is_empty() {
            output.push_str(&format!("LANGUAGES\n"));
            output.push_str(&format!("───────────────────────────────────────────────────────────────\n"));
            for (lang, coverage) in &self.languages {
                output.push_str(&format!("  {}: {} files, {} lines, {:.1}% compliant\n",
                    lang, coverage.file_count, coverage.total_lines, coverage.compliance_percentage));
            }
            output.push_str("\n");
        }

        if !self.illegal_patterns.is_empty() {
            output.push_str(&format!("ILLEGAL PATTERNS (LAW #4 VIOLATIONS)\n"));
            output.push_str(&format!("───────────────────────────────────────────────────────────────\n"));
            for (i, pattern) in self.illegal_patterns.iter().take(20).enumerate() {
                output.push_str(&format!("  {}. [{:?}] {}:{}:{}\n",
                    i + 1,
                    pattern.pattern_type,
                    pattern.location.file.display(),
                    pattern.location.line,
                    pattern.location.column));
                output.push_str(&format!("     {}\n", pattern.code_snippet.lines().next().unwrap_or("")));
                if let Some(ref suggestion) = pattern.suggested_refactoring {
                    output.push_str(&format!("     Suggestion: {}\n", suggestion));
                }
            }
            if self.illegal_patterns.len() > 20 {
                output.push_str(&format!("  ... and {} more\n", self.illegal_patterns.len() - 20));
            }
            output.push_str("\n");
        }

        output.push_str(&format!("═══════════════════════════════════════════════════════════════\n"));

        output
    }
}

impl Default for ComplianceSummary {
    fn default() -> Self {
        Self {
            total_files: 0,
            parsed_successfully: 0,
            parse_failures: 0,
            compliance_percentage: 0.0,
            total_nodes_analyzed: 0,
            legal_pattern_count: 0,
            illegal_pattern_count: 0,
            pattern_compliance_percentage: 100.0,
            refactoring_effort: RefactoringEffort::Low,
            verdict: ComplianceVerdict::FullyCompliant,
        }
    }
}

impl Default for NodeStatistics {
    fn default() -> Self {
        Self {
            total_nodes: 0,
            by_type: HashMap::new(),
            total_edges: 0,
            edges_by_type: HashMap::new(),
            loc_covered: 0,
            loc_orphaned: 0,
            coverage_percentage: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verdict_calculation() {
        let mut report = ComplianceReport::new(PathBuf::from("/test"));

        // Add 100 files, 98 successful
        for i in 0..98 {
            report.files.push(FileReport {
                path: PathBuf::from(format!("file{}.rs", i)),
                language: "rust".to_string(),
                parse_result: ParseResult::Success,
                line_count: 100,
                node_count: 50,
                illegal_patterns: vec![],
            });
        }
        for i in 98..100 {
            report.files.push(FileReport {
                path: PathBuf::from(format!("file{}.rs", i)),
                language: "rust".to_string(),
                parse_result: ParseResult::Failed { error: "test".to_string() },
                line_count: 100,
                node_count: 0,
                illegal_patterns: vec![],
            });
        }

        report.calculate_summary();

        assert_eq!(report.summary.compliance_percentage, 98.0);
        assert_eq!(report.summary.verdict, ComplianceVerdict::FullyCompliant);
    }
}
