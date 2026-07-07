use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

/// Rosetta — OpenAI-to-ACP HTTP proxy.
///
/// Every flag falls back to its `ROSETTA_*` environment variable when omitted.
/// Precedence, highest to lowest: CLI flag > environment variable > built-in default.
#[derive(Debug, Parser)]
#[command(
    name = "rosetta",
    version,
    about = "OpenAI-to-ACP HTTP proxy",
    long_about = None,
)]
pub struct Cli {
    /// Command used to spawn the ACP agent.
    ///
    /// Falls back to $ROSETTA_ACP_COMMAND, then to "opencode".
    #[arg(
        short = 'c',
        long = "acp-command",
        env = "ROSETTA_ACP_COMMAND",
        default_value = "opencode",
        value_name = "COMMAND",
    )]
    pub acp_command: String,

    /// Arguments forwarded to the ACP agent.
    ///
    /// Accepts multiple `--acp-arg <VALUE>` occurrences on the CLI, a single
    /// space-separated string (quote it on the shell), or a space-separated
    /// $ROSETTA_ACP_ARGS. Falls back to ["acp"] when nothing is provided.
    #[arg(
        short = 'a',
        long = "acp-arg",
        env = "ROSETTA_ACP_ARGS",
        value_delimiter = ' ',
        default_values_t = [String::from("acp")],
        value_name = "ARG",
    )]
    pub acp_args: Vec<String>,

    /// Working directory sent to the agent in `session/new`.
    ///
    /// Falls back to $ROSETTA_CWD, then to the process working directory at startup.
    #[arg(
        short = 'w',
        long = "cwd",
        env = "ROSETTA_CWD",
        value_name = "PATH",
    )]
    pub cwd: Option<PathBuf>,

    /// MCP server configurations, as a JSON array.
    ///
    /// Passed through the ACP-standard `mcpServers` field of `session/new`.
    /// Falls back to $ROSETTA_MCP_SERVERS, then to [] (no MCP servers).
    /// Malformed JSON aborts the process at startup with a clear error.
    #[arg(
        short = 'm',
        long = "mcp-servers",
        env = "ROSETTA_MCP_SERVERS",
        value_parser = parse_mcp_servers,
        value_name = "JSON",
    )]
    pub mcp_servers: Option<McpServersJson>,

    /// HTTP listen address for the proxy server.
    ///
    /// Falls back to $ROSETTA_LISTEN, then to 0.0.0.0:3000.
    #[arg(
        short = 'l',
        long = "listen",
        env = "ROSETTA_LISTEN",
        default_value = "0.0.0.0:3000",
        value_name = "HOST:PORT",
    )]
    pub listen: SocketAddr,
}

/// Newtype wrapper around the parsed MCP servers JSON array.
///
/// clap's derive macro special-cases bare `Vec<T>`/`Option<Vec<T>>` field
/// types to mean "collect one `T` per flag occurrence". Our `value_parser`
/// already parses a single `--mcp-servers` occurrence into a whole
/// `Vec<serde_json::Value>`, which collides with that built-in behavior and
/// causes an internal type-downcast panic. Wrapping the Vec in a newtype
/// makes the field a plain single-value `Option<McpServersJson>` to clap,
/// sidestepping the ambiguity.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServersJson(pub Vec<serde_json::Value>);

pub struct ResolvedConfig {
    pub acp_command: String,
    pub acp_args: Vec<String>,
    pub cwd: String,
    pub mcp_servers: Vec<serde_json::Value>,
    pub listen: SocketAddr,
}

impl Cli {
    /// Consumes the parsed CLI and materializes runtime defaults for optional fields
    /// (`cwd` falls back to `std::env::current_dir()`, `mcp_servers` to an empty Vec).
    pub fn resolve(self) -> ResolvedConfig {
        let cwd = self
            .cwd
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
            .to_string_lossy()
            .into_owned();

        ResolvedConfig {
            acp_command: self.acp_command,
            acp_args: self.acp_args,
            cwd,
            mcp_servers: self.mcp_servers.map(|w| w.0).unwrap_or_default(),
            listen: self.listen,
        }
    }
}

fn parse_mcp_servers(s: &str) -> Result<McpServersJson, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(McpServersJson(Vec::new()));
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("invalid JSON for MCP servers: {e}"))?;
    match value {
        serde_json::Value::Array(arr) => Ok(McpServersJson(arr)),
        other => Err(format!(
            "expected a JSON array of MCP server objects, got: {other}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn defaults_apply_when_nothing_provided() {
        let cli = Cli::try_parse_from(["rosetta"]).unwrap();
        assert_eq!(cli.acp_command, "opencode");
        assert_eq!(cli.acp_args, vec!["acp".to_string()]);
        assert_eq!(cli.cwd, None);
        assert_eq!(cli.mcp_servers, None);
        assert_eq!(cli.listen.to_string(), "0.0.0.0:3000");
    }

    #[test]
    fn cli_flags_populate_all_fields() {
        let cli = Cli::try_parse_from([
            "rosetta",
            "--acp-command",
            "python3",
            "--acp-arg",
            "script.py",
            "--acp-arg",
            "extra",
            "--cwd",
            "/tmp/work",
            "--listen",
            "127.0.0.1:8080",
        ])
        .unwrap();
        assert_eq!(cli.acp_command, "python3");
        assert_eq!(cli.acp_args, vec!["script.py", "extra"]);
        assert_eq!(cli.cwd, Some(PathBuf::from("/tmp/work")));
        assert_eq!(cli.listen.to_string(), "127.0.0.1:8080");
    }

    #[test]
    fn acp_arg_accepts_dash_prefixed_value_via_equals_syntax() {
        let cli = Cli::try_parse_from([
            "rosetta",
            "--acp-arg=script.py",
            "--acp-arg=--verbose",
        ])
        .unwrap();
        assert_eq!(cli.acp_args, vec!["script.py", "--verbose"]);
    }

    #[test]
    fn acp_arg_accepts_quoted_space_separated_string() {
        let cli = Cli::try_parse_from(["rosetta", "--acp-arg", "acp --debug"]).unwrap();
        assert_eq!(cli.acp_args, vec!["acp", "--debug"]);
    }

    #[test]
    fn parse_mcp_servers_accepts_valid_array() {
        let result = parse_mcp_servers(r#"[{"name":"a"},{"name":"b"}]"#).unwrap();
        assert_eq!(result.0.len(), 2);
    }

    /// Regression test: parsing --mcp-servers through the REAL clap derive
    /// pipeline (not calling parse_mcp_servers directly) previously panicked
    /// with a clap internal type-downcast mismatch when the field type was
    /// `Option<Vec<serde_json::Value>>` combined with a custom value_parser
    /// that itself returns a Vec. See McpServersJson newtype fix below.
    #[test]
    fn cli_parses_mcp_servers_flag_without_panicking() {
        let cli = Cli::try_parse_from([
            "rosetta",
            "--mcp-servers",
            r#"[{"name":"fs","command":"mcp-fs"}]"#,
        ])
        .expect("clap should parse --mcp-servers without panicking");
        let servers = cli.resolve().mcp_servers;
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["name"], "fs");
    }

    #[test]
    fn parse_mcp_servers_accepts_empty_string() {
        assert_eq!(parse_mcp_servers("").unwrap(), McpServersJson(Vec::new()));
        assert_eq!(parse_mcp_servers("   ").unwrap(), McpServersJson(Vec::new()));
    }

    #[test]
    fn parse_mcp_servers_rejects_non_array() {
        let err = parse_mcp_servers(r#"{"name":"a"}"#).unwrap_err();
        assert!(err.contains("expected a JSON array"), "unexpected error: {err}");
    }

    #[test]
    fn parse_mcp_servers_rejects_invalid_json() {
        let err = parse_mcp_servers("not json").unwrap_err();
        assert!(err.contains("invalid JSON"), "unexpected error: {err}");
    }

    #[test]
    fn resolve_falls_back_to_current_dir_when_cwd_none() {
        let cli = Cli::try_parse_from(["rosetta"]).unwrap();
        let cfg = cli.resolve();
        assert!(!cfg.cwd.is_empty());
    }

    #[test]
    fn resolve_preserves_all_fields() {
        let cli = Cli::try_parse_from([
            "rosetta",
            "--acp-command",
            "opencode",
            "--acp-arg",
            "acp",
            "--cwd",
            "/tmp/rosetta",
            "--listen",
            "127.0.0.1:4242",
        ])
        .unwrap();
        let cfg = cli.resolve();
        assert_eq!(cfg.acp_command, "opencode");
        assert_eq!(cfg.acp_args, vec!["acp".to_string()]);
        assert_eq!(cfg.cwd, "/tmp/rosetta");
        assert_eq!(cfg.listen.to_string(), "127.0.0.1:4242");
        assert!(cfg.mcp_servers.is_empty());
    }

    #[test]
    fn resolve_uses_provided_cwd_verbatim() {
        let cli = Cli::try_parse_from(["rosetta", "--cwd", "/tmp/rosetta-test"]).unwrap();
        let cfg = cli.resolve();
        assert_eq!(cfg.cwd, "/tmp/rosetta-test");
    }
}
