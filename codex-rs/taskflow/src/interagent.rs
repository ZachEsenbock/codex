//! Inter-agent message protocol and parser.
//!
//! Convention: an agent can emit a routed message to a peer by printing a
//! delimited block to stdout:
//!
//! ```text
//! @@to: <task_id>
//! <message body (any text)>
//! @@/to
//! ```
//!
//! - `<task_id>` is the target Task ID as known to the runner.
//! - The body may span multiple lines. The block ends at a line that is exactly
//!   `@@/to` (ignoring surrounding whitespace).
//! - Blocks cannot be nested; any `@@to:` inside the body is treated as plain text.
//! - Multiple blocks can appear in the output; each is parsed independently.
//!
//! The runner detects these blocks in the sub-agent's stdout stream and routes
//! them as guidance injections to the target agent.

use regex::Regex;

/// A parsed routed message destined for another task/agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedMessage {
    pub to: String,
    pub body: String,
}

/// Streaming parser for routed message blocks. Feed lines as they arrive; when
/// a block closes, `feed_line` returns `Some(RoutedMessage)`.
#[derive(Debug, Default)]
pub struct InterAgentParser {
    in_block: bool,
    target: String,
    buf: String,
}

impl InterAgentParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one logical line (without trailing `\n`). Returns a parsed
    /// `RoutedMessage` when a block is completed.
    pub fn feed_line(&mut self, line: &str) -> Option<RoutedMessage> {
        if !self.in_block {
            if let Some(target) = parse_open(line) {
                self.in_block = true;
                self.target = target;
                self.buf.clear();
                return None;
            }
            return None;
        }

        if is_close(line) {
            let msg = RoutedMessage {
                to: self.target.clone(),
                body: self.buf.clone(),
            };
            self.in_block = false;
            self.target.clear();
            self.buf.clear();
            return Some(msg);
        }

        if !self.buf.is_empty() {
            self.buf.push('\n');
        }
        self.buf.push_str(line);
        None
    }
}

fn parse_open(line: &str) -> Option<String> {
    // Accept leading/trailing whitespace; capture everything after `@@to:` as the id,
    // trimming whitespace.
    // Example: "@@to: backend-2" => target "backend-2"
    // Use a small regex for clarity and robustness.
    static OPEN_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = OPEN_RE.get_or_init(|| Regex::new(r"^\s*@@to:\s*(?P<id>.+?)\s*$").unwrap());
    re.captures(line)
        .and_then(|c| c.name("id").map(|m| m.as_str().trim().to_string()))
        .filter(|s| !s.is_empty())
}

fn is_close(line: &str) -> bool {
    line.trim() == "@@/to"
}

/// Parse all routed messages from a single string. Useful for tests.
pub fn parse_all(input: &str) -> Vec<RoutedMessage> {
    let mut p = InterAgentParser::new();
    let mut out = Vec::new();
    for line in input.split('\n') {
        if let Some(msg) = p.feed_line(line) {
            out.push(msg);
        }
    }
    out
}

/// Short blurb included in the role primer to inform sub-agents about peers and
/// how to route messages between tasks.
pub const PROTOCOL_PRIMER: &str = "Peers and routing: Multiple sub-agents may run concurrently with unique Task IDs. To send guidance to a peer, print a routed block to stdout:\n\n@@to: <task_id>\n<message>\n@@/to\n\nThe runner will deliver the message to the target agent and continue your run.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_block() {
        let input = "hello\n@@to: A-1\nplan x\nrun y\n@@/to\nbye";
        let msgs = parse_all(input);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].to, "A-1");
        assert_eq!(msgs[0].body, "plan x\nrun y");
    }

    #[test]
    fn parses_multiple_blocks() {
        let input = "@@to: 1\none\n@@/to\n@@to: 2\n two \n@@/to";
        let msgs = parse_all(input);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].to, "1");
        assert_eq!(msgs[0].body, "one");
        assert_eq!(msgs[1].to, "2");
        assert_eq!(msgs[1].body, " two ");
    }

    #[test]
    fn ignores_close_when_not_in_block() {
        let input = "@@/to\n@@to: X\nhello\n@@/to";
        let msgs = parse_all(input);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].to, "X");
        assert_eq!(msgs[0].body, "hello");
    }

    #[test]
    fn open_line_tolerates_whitespace() {
        let input = "  @@to:   target-42  \nmsg\n  @@/to  ";
        let msgs = parse_all(input);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].to, "target-42");
        assert_eq!(msgs[0].body, "msg");
    }
}
