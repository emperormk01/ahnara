//! Skill Installer - Auto-install skills from natural language

use anyhow::{bail, Result};
use std::path::PathBuf;
use std::process::Command;

use super::registry::{SkillRegistry, RegistrySkill};

/// Skill installer
pub struct SkillInstaller {
    skills_dir: PathBuf,
    registry: SkillRegistry,
}

impl SkillInstaller {
    pub fn new(skills_dir: PathBuf) -> Self {
        Self {
            skills_dir,
            registry: SkillRegistry::new(),
        }
    }

    /// Install a skill by name (searches registry)
    pub async fn install(&mut self, name: &str) -> Result<String> {
        // Check if already installed
        if self.is_installed(name) {
            return Ok(format!("Skill '{}' is already installed", name));
        }

        // Search registry
        let skill = self.registry.get(name).await?
            .ok_or_else(|| anyhow::anyhow!("Skill '{}' not found in registry", name))?;

        self.install_from_registry(&skill).await
    }

    /// Update a skill by reinstalling it from the registry
    pub async fn update(&mut self, name: &str) -> Result<String> {
        if self.is_installed(name) {
            self.uninstall(name)?;
        }
        self.install(name).await
    }

    /// Install from GitHub URL
    pub async fn install_from_url(&self, url: &str) -> Result<String> {
        self.registry.install_from_github(url, &self.skills_dir).await
    }

    /// Install from registry entry
    async fn install_from_registry(&self, skill: &RegistrySkill) -> Result<String> {
        let skill_dir = self.skills_dir.join(&skill.name);
        std::fs::create_dir_all(&skill_dir)?;

        // Create SKILL.md
        let skill_content = self.fetch_skill_content(&skill.github_url).await?;
        std::fs::write(skill_dir.join("SKILL.md"), skill_content)?;

        Ok(skill.name.clone())
    }

    /// Fetch skill content from GitHub
    async fn fetch_skill_content(&self, github_url: &str) -> Result<String> {
        self.registry.install_from_github(github_url, &self.skills_dir).await?;
        
        // Read the installed SKILL.md
        let skill_name = github_url.split('/').last().unwrap_or("skill");
        let skill_file = self.skills_dir.join(skill_name).join("SKILL.md");
        
        if skill_file.exists() {
            return Ok(std::fs::read_to_string(skill_file)?);
        }

        // Return a default template
        Ok(format!(r#"---
name: {}
description: {}
---

# Skill Instructions

This skill was installed from the registry.

## Usage

Refer to the skill documentation for usage instructions.
"#, github_url.split('/').last().unwrap_or("unknown"), "Installed from registry"))
    }

    /// Check if a skill is already installed
    pub fn is_installed(&self, name: &str) -> bool {
        self.skills_dir.join(name).join("SKILL.md").exists()
    }

    /// Uninstall a skill
    pub fn uninstall(&self, name: &str) -> Result<()> {
        let skill_dir = self.skills_dir.join(name);
        
        if skill_dir.exists() {
            std::fs::remove_dir_all(skill_dir)?;
        }

        Ok(())
    }

    /// Search for skills matching a query
    pub async fn search(&mut self, query: &str) -> Result<Vec<RegistrySkill>> {
        self.registry.search(query).await
    }

    /// List all available skills in registry
    pub async fn list_available(&mut self) -> Result<Vec<RegistrySkill>> {
        self.registry.list().await
    }

    /// Auto-install skill based on natural language prompt
    /// Returns the installed skill name or None if no match found
    pub async fn auto_install(&mut self, prompt: &str) -> Result<Option<String>> {
        // Extract potential skill requirements from prompt
        let keywords = extract_skill_keywords(prompt);

        for keyword in keywords {
            let results = self.search(&keyword).await?;
            
            if let Some(skill) = results.first() {
                // Install the first matching skill
                match self.install(&skill.name).await {
                    Ok(name) => return Ok(Some(name)),
                    Err(e) => {
                        tracing::warn!("Failed to auto-install skill '{}': {}", skill.name, e);
                        continue;
                    }
                }
            }
        }

        Ok(None)
    }

    /// Install skill from local template
    pub fn create_from_template(&self, name: &str, description: &str) -> Result<PathBuf> {
        let skill_dir = self.skills_dir.join(name);
        std::fs::create_dir_all(&skill_dir)?;

        let content = format!(r#"---
name: {}
description: {}
metadata:
  author: "user"
  version: "1.0.0"
---

# {}

## Instructions

Add your skill instructions here.

## Steps

1. Step 1
2. Step 2
3. Step 3

## Examples

```bash
# Example commands
```

## Notes

- Important notes
- Edge cases to handle
"#, name, description, name);

        std::fs::write(skill_dir.join("SKILL.md"), content)?;

        // Create optional directories
        std::fs::create_dir_all(skill_dir.join("scripts"))?;
        std::fs::create_dir_all(skill_dir.join("references"))?;

        Ok(skill_dir)
    }

    /// Install from a git repository
    pub fn install_from_git(&self, repo_url: &str) -> Result<String> {
        let skills_dir = self.skills_dir.parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid skills directory"))?;

        // Clone the repo temporarily
        let temp_dir = std::env::temp_dir().join("ahnara-skill-clone");
        let _ = std::fs::remove_dir_all(&temp_dir);
        
        let status = Command::new("git")
            .args(["clone", "--depth", "1", repo_url, &temp_dir.to_string_lossy()])
            .status()?;

        if !status.success() {
            bail!("Failed to clone repository: {}", repo_url);
        }

        // Find SKILL.md files and copy them
        let mut installed = Vec::new();
        for entry in walkdir::WalkDir::new(&temp_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_name() == "SKILL.md" {
                if let Some(skill_dir) = entry.path().parent() {
                    if let Some(skill_name) = skill_dir.file_name() {
                        let dest = skills_dir.join(skill_name);
                        let _ = std::fs::remove_dir_all(&dest);
                        std::fs::create_dir_all(&dest)?;
                        
                        // Copy all files in skill directory
                        for file in std::fs::read_dir(skill_dir)? {
                            let file = file?;
                            let dest_file = dest.join(file.file_name());
                            std::fs::copy(file.path(), dest_file)?;
                        }
                        
                        installed.push(skill_name.to_string_lossy().to_string());
                    }
                }
            }
        }

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);

        if installed.is_empty() {
            bail!("No SKILL.md files found in repository");
        }

        Ok(installed.join(", "))
    }
}

impl Default for SkillInstaller {
    fn default() -> Self {
        Self::new(
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("ahnara")
                .join("skills")
        )
    }
}

/// Extract potential skill keywords from a natural language prompt
fn extract_skill_keywords(prompt: &str) -> Vec<String> {
    let prompt_lower = prompt.to_lowercase();
    let mut keywords = Vec::new();

    // Code-related
    if prompt_lower.contains("code review") || prompt_lower.contains("review code") {
        keywords.push("code-review".into());
    }
    if prompt_lower.contains("test") || prompt_lower.contains("testing") {
        keywords.push("test-driven-development".into());
    }
    if prompt_lower.contains("debug") || prompt_lower.contains("fix bug") {
        keywords.push("systematic-debugging".into());
    }

    // Research-related
    if prompt_lower.contains("paper") || prompt_lower.contains("arxiv") || prompt_lower.contains("research") {
        keywords.push("arxiv".into());
    }
    if prompt_lower.contains("scrape") || prompt_lower.contains("web scraping") {
        keywords.push("web-scraping".into());
    }

    // ML/AI-related
    if prompt_lower.contains("fine-tune") || prompt_lower.contains("finetune") || prompt_lower.contains("train") {
        keywords.push("fine-tuning-axolotl".into());
    }
    if prompt_lower.contains("prompt") {
        keywords.push("prompt-engineering".into());
    }

    // DevOps-related
    if prompt_lower.contains("docker") || prompt_lower.contains("container") {
        keywords.push("docker-deployment".into());
    }
    if prompt_lower.contains("deploy") || prompt_lower.contains("deployment") {
        keywords.push("docker-deployment".into());
    }

    // Git-related
    if prompt_lower.contains("git") || prompt_lower.contains("branch") || prompt_lower.contains("commit") {
        keywords.push("git-workflow".into());
    }

    // API-related
    if prompt_lower.contains("api") || prompt_lower.contains("integrate") {
        keywords.push("api-integration".into());
    }

    // If no specific keywords found, try to extract nouns
    if keywords.is_empty() {
        let words: Vec<&str> = prompt.split_whitespace().collect();
        for word in words {
            if word.len() > 4 && !COMMON_WORDS.contains(&word.to_lowercase().as_str()) {
                keywords.push(word.to_lowercase());
            }
        }
    }

    keywords
}

const COMMON_WORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "can", "her",
    "was", "one", "our", "out", "has", "have", "been", "will", "with", "from",
    "this", "that", "what", "when", "where", "which", "would", "could", "should",
    "want", "need", "like", "just", "also", "make", "more", "some", "than",
    "them", "then", "they", "very", "about", "after", "before", "into", "over",
];