use std::{
    process::{
        Child,
        Command,
        ExitStatus,
        Stdio,
    },
    result::Result as StdResult,
};

use miette::{
    IntoDiagnostic,
    Result,
};
use tracing::{
    event,
    instrument,
};

#[derive(Debug)]
pub struct ProcessManager {
    command: String,
    process: Option<StdResult<Child, ExitStatus>>,
}

impl From<Vec<String>> for ProcessManager {
    fn from(value: Vec<String>) -> Self {
        let joined_command: Vec<&str> = value.iter().map(String::as_str).collect();
        let command = shellwords::join(&joined_command);
        Self::new(&command)
    }
}

impl ProcessManager {
    // Panic in Result function for compatibility with non result functions
    #[allow(clippy::panic_in_result_fn)]
    #[instrument(fields(command = %self.command))]
    pub fn check_if_exit_successful(&mut self) -> Option<bool> {
        match self.process {
            None => None,
            Some(Ok(ref mut child)) => {
                if let Some(status) = child.try_wait().into_diagnostic().ok().flatten() {
                    self.process = None;
                    Some(status.success())
                } else {
                    None
                }
            }
            Some(Err(status)) => Some(status.success()),
        }
    }

    // Panic in Result function for compatibility with non result functions
    #[allow(clippy::panic_in_result_fn)]
    #[instrument(fields(command = %self.command))]
    pub(crate) fn stop(&mut self) -> Result<()> {
        match self.process {
            Some(Err(_)) | None => {
                event!(tracing::Level::TRACE, "No process running");
                Ok(())
            }
            Some(Ok(ref mut child)) => {
                event!(tracing::Level::INFO, "Stopping process");
                child.kill().into_diagnostic()?;
                self.process = Some(Err(child.wait().into_diagnostic()?));

                Ok(())
            }
        }
    }

    // Panic in Result function for compatibility with non result functions
    #[allow(clippy::panic_in_result_fn)]
    #[instrument(fields(command = %self.command))]
    pub(crate) fn start(&mut self) -> Result<()> {
        if self.process.is_some() {
            return Ok(());
        }

        let arguments = shellwords::split(&self.command).into_diagnostic()?;
        let child = Command::new(&arguments[0])
            .args(&arguments[1..])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .into_diagnostic()?;
        self.process = Some(Ok(child));

        Ok(())
    }

    // Panic in Result function for compatiblity with non result functions
    #[allow(clippy::panic_in_result_fn)]
    #[instrument(fields(command = %self.command))]
    pub fn check_if_running(&mut self) -> bool {
        match self.process {
            Some(Err(_)) | None => false,
            Some(Ok(ref mut child)) => child.try_wait().unwrap_or(None).is_none(),
        }
    }

    pub fn new(command: &str) -> Self {
        Self {
            command: command.to_string(),
            process: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env::temp_dir,
        fs,
        thread::sleep,
        time::Duration,
    };

    use super::*;

    #[test]
    fn can_start_and_stop_a_process() {
        let mut process = ProcessManager::new("sleep 30");
        process.start().expect("Failed to start process");
        assert!(process.check_if_running());
        process.stop().expect("Failed to stop process");
        assert!(!process.check_if_running());
        assert!(!process
            .check_if_exit_successful()
            .expect("Failed to get process status"));
    }

    #[test]
    fn process_can_run_to_completion() {
        let temp_file = temp_dir().join("test_can_launch_a_process");
        if temp_file.exists() {
            fs::remove_file(temp_file.clone()).expect("Failed to remove file");
        }

        let cmd = shellwords::join(&[
            "bash",
            "-c",
            &format!(
                "echo 'Hello World' > {}",
                temp_file
                    .to_str()
                    .expect("Failed to convert path to string")
            ),
        ]);
        let mut process = ProcessManager::new(&cmd);
        process.start().expect("Failed to start process");

        while process.check_if_running() {
            sleep(Duration::from_millis(100));
        }

        assert!(!process.check_if_running());
        assert!(temp_file.exists());
        assert!(process
            .check_if_exit_successful()
            .expect("Failed to get process status"));
    }

    #[test]
    fn can_tell_identify_failed_process() {
        let cmd = shellwords::join(&["bash", "-c", "exit 1"]);
        let mut process = ProcessManager::new(&cmd);
        process.start().expect("Failed to start process");

        while process.check_if_running() {
            sleep(Duration::from_millis(100));
        }

        assert!(!process.check_if_running());
        assert!(!process
            .check_if_exit_successful()
            .expect("Failed to get process status"));
    }

    #[test]
    fn can_be_created_from_vec_string() {
        let cmd = vec!["bash".to_string(), "-c".to_string(), "exit 1".to_string()];
        let mut process = ProcessManager::from(cmd);
        assert!(!process.check_if_running());
    }
}
