use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::{debug, error};

use crate::AcpError;

/// Manages a child process and communicates with it via newline-delimited JSON over stdio.
pub struct AcpTransport {
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    child: Child,
}

impl AcpTransport {
    /// Spawn a new child process and wrap its stdio pipes.
    pub async fn new(program: &str, args: &[&str]) -> Result<Self, AcpError> {
        Self::new_with_env(program, args, &[]).await
    }

    /// Spawn a new child process with extra environment variables.
    pub async fn new_with_env(
        program: &str,
        args: &[&str],
        env_vars: &[(String, String)],
    ) -> Result<Self, AcpError> {
        debug!(%program, ?args, "Spawning ACP transport process");

        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        for (k, v) in env_vars {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(AcpError::Io)?;

        let stdin = child.stdin.take().ok_or_else(|| {
            AcpError::Protocol {
                message: "Failed to acquire child stdin".to_string(),
            }
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AcpError::Protocol {
                message: "Failed to acquire child stdout".to_string(),
            }
        })?;

        Ok(Self {
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            child,
        })
    }

    /// Send a raw message line to the child process (appends newline).
    pub async fn send_message(&mut self, msg: &str) -> Result<(), AcpError> {
        self.stdin
            .write_all(msg.as_bytes())
            .await
            .map_err(AcpError::Io)?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(AcpError::Io)?;
        self.stdin.flush().await.map_err(AcpError::Io)?;
        Ok(())
    }

    /// Read a single line from the child process stdout.
    /// Returns `None` when the stream has closed.
    pub async fn read_line(&mut self) -> Result<Option<String>, AcpError> {
        let mut line = String::new();
        let bytes_read = self
            .stdout
            .read_line(&mut line)
            .await
            .map_err(AcpError::Io)?;

        if bytes_read == 0 {
            return Ok(None);
        }

        // Strip trailing newline characters.
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }

        Ok(Some(line))
    }

    /// Terminate the child process.
    pub async fn shutdown(mut self) -> Result<(), AcpError> {
        debug!("Shutting down ACP transport");
        if let Err(e) = self.child.start_kill() {
            error!(error = %e, "Failed to send kill signal to child");
        }
        let _ = self.child.wait().await;
        Ok(())
    }
}
