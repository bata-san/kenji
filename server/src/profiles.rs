use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use portable_pty::CommandBuilder;
use sha2::{Digest, Sha256};

/// Per-Kenji-user configuration roots for CLI tools launched in terminal PTYs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliProfile {
    opencode_data: PathBuf,
    opencode_config: PathBuf,
    claude: PathBuf,
    omp: PathBuf,
}

impl CliProfile {
    pub fn ensure(data_dir: &Path, username: &str) -> Result<Self> {
        let id = hex::encode(Sha256::digest(username.as_bytes()));
        let root = data_dir.join("profiles").join(id);
        let profile = Self {
            opencode_data: root.join("opencode-data"),
            opencode_config: root.join("opencode-config"),
            claude: root.join("claude"),
            omp: root.join("omp"),
        };
        for path in [
            &profile.opencode_data,
            &profile.opencode_config,
            &profile.claude,
            &profile.omp,
        ] {
            std::fs::create_dir_all(path)
                .with_context(|| format!("CLIプロファイルを作成できません: {}", path.display()))?;
        }
        Ok(profile)
    }

    /// Override supported tool roots and prevent ambient provider API keys
    /// inherited from the Kenji server process from taking precedence.
    pub fn configure_command(&self, command: &mut CommandBuilder) {
        command.env("XDG_DATA_HOME", &self.opencode_data);
        command.env("OPENCODE_CONFIG_DIR", &self.opencode_config);
        command.env("CLAUDE_CONFIG_DIR", &self.claude);
        command.env("PI_CONFIG_DIR", &self.omp);
        command.env_remove("PI_CODING_AGENT_DIR");

        for key in INHERITED_CREDENTIALS {
            command.env_remove(key);
        }
    }
}

const INHERITED_CREDENTIALS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "OPENAI_ORG_ID",
    "OPENAI_PROJECT_ID",
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AZURE_OPENAI_API_KEY",
    "AZURE_OPENAI_AD_TOKEN",
    "GROQ_API_KEY",
    "MISTRAL_API_KEY",
    "DEEPSEEK_API_KEY",
    "OPENROUTER_API_KEY",
    "CEREBRAS_API_KEY",
    "XAI_API_KEY",
    "COHERE_API_KEY",
    "HF_TOKEN",
    "HUGGINGFACE_HUB_TOKEN",
];

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn users_get_distinct_persistent_cli_roots() {
        let temp = tempdir().unwrap();
        let alice = CliProfile::ensure(temp.path(), "alice").unwrap();
        let bob = CliProfile::ensure(temp.path(), "bob").unwrap();
        assert_ne!(alice, bob);
        assert!(alice.opencode_data.is_dir());
        assert!(alice.opencode_config.is_dir());
        assert!(alice.claude.is_dir());
        assert!(alice.omp.is_dir());
        assert_eq!(alice, CliProfile::ensure(temp.path(), "alice").unwrap());
    }

    #[test]
    fn inherited_credentials_are_removed_and_tool_roots_are_explicit() {
        let temp = tempdir().unwrap();
        let profile = CliProfile::ensure(temp.path(), "alice").unwrap();
        let mut command = CommandBuilder::new("cmd.exe");
        profile.configure_command(&mut command);

        for key in INHERITED_CREDENTIALS {
            assert!(command.get_env(key).is_none(), "{key}");
        }
        assert_eq!(
            command.get_env("XDG_DATA_HOME"),
            Some(profile.opencode_data.as_os_str())
        );
        assert_eq!(
            command.get_env("OPENCODE_CONFIG_DIR"),
            Some(profile.opencode_config.as_os_str())
        );
        assert_eq!(
            command.get_env("CLAUDE_CONFIG_DIR"),
            Some(profile.claude.as_os_str())
        );
        assert_eq!(
            command.get_env("PI_CONFIG_DIR"),
            Some(profile.omp.as_os_str())
        );
    }
}
