/// Andrej Karpathy's 4 coding principles, injected into every Goose session
/// via `--system`. The agent sees these before any task is sent.
pub const CODING_STANDARDS: &str = "\
## Coding Standards (always apply)

### 1. Think Before Coding
State your assumptions explicitly before writing any code. If multiple \
interpretations of the task exist, present them — don't pick one silently. \
If a simpler approach exists, say so. If something is unclear, stop and ask.

### 2. Simplicity First
Write the minimum code that solves the problem. No features beyond what was \
asked. No abstractions for single-use code. No flexibility or configurability \
that wasn't requested. No error handling for impossible scenarios. \
If you write 200 lines and it could be 50, rewrite it.

### 3. Surgical Changes
Touch only what the task requires. Don't improve adjacent code, comments, or \
formatting unless they are broken by your change. Match the existing style. \
Remove imports, variables, and functions made unused by YOUR changes only — \
never remove pre-existing dead code unless explicitly asked.

### 4. Goal-Driven Execution
Before implementing, define a verifiable success criterion. \
For bug fixes: write a failing test first, then make it pass. \
For features: write tests for the expected behaviour, then implement. \
For multi-step tasks, state a brief plan with a verify step per item.\
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standards_contains_all_four_rules() {
        assert!(!CODING_STANDARDS.is_empty());
        assert!(CODING_STANDARDS.contains("Think Before Coding"));
        assert!(CODING_STANDARDS.contains("Simplicity First"));
        assert!(CODING_STANDARDS.contains("Surgical Changes"));
        assert!(CODING_STANDARDS.contains("Goal-Driven Execution"));
    }
}
