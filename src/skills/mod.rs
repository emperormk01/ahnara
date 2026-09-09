//! Skill System - Markdown-based skills with progressive disclosure
//! 
//! Compatible with agentskills.io specification
//! 
//! Key Features:
//! - Progressive disclosure (4-tier loading)
//! - Natural language skill installation
//! - Self-improvement loop
//! - Conditional activation based on available tools
//! - External skill directories

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::RwLock;

pub mod registry;
pub mod installer;
pub mod extractor;

pub use registry::SkillRegistry;
pub use installer::SkillInstaller;
pub use extractor::{ExtractorConfig, SkillExtractor, ToolTraceEntry};

/// Skill metadata (frontmatter)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub compatibility: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    #[serde(default)]
    pub allowed_tools: Option<String>,
    // Hermes-specific extensions
    #[serde(default)]
    pub platforms: Option<Vec<String>>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default, rename = "requires_tools")]
    pub requires_tools: Option<Vec<String>>,
    #[serde(default, rename = "fallback_for_tools")]
    pub fallback_for_tools: Option<Vec<String>>,
    #[serde(default, rename = "requires_toolsets")]
    pub requires_toolsets: Option<Vec<String>>,
    #[serde(default, rename = "fallback_for_toolsets")]
    pub fallback_for_toolsets: Option<Vec<String>>,
}

/// Skill with full content
#[derive(Debug, Clone)]
pub struct Skill {
    pub meta: SkillMeta,
    pub body: String,
    pub path: PathBuf,
    pub is_external: bool,
}

impl Skill {
    /// Parse a SKILL.md file content
    pub fn parse(content: &str) -> Result<Self> {
        // Extract frontmatter
        if !content.starts_with("---") {
            bail!("Skill must start with YAML frontmatter");
        }
        
        let end_idx = content[3..].find("---")
            .ok_or_else(|| anyhow::anyhow!("Frontmatter not closed"))?;
        
        let frontmatter = &content[3..end_idx + 3];
        let body = &content[end_idx + 6..];
        
        let meta: SkillMeta = serde_yaml::from_str(frontmatter)?;
        
        Ok(Self {
            meta,
            body: body.to_string(),
            path: PathBuf::new(), // Will be set by caller if needed
            is_external: false,
        })
    }
    
    /// Parse from file path
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut skill = Self::parse(&content)?;
        skill.path = path.to_path_buf();
        Ok(skill)
    }
    
    /// Get skill name
    pub fn name(&self) -> &str {
        &self.meta.name
    }
    
    /// Get skill description
    pub fn description(&self) -> &str {
        &self.meta.description
    }
}

/// Skill index entry (Tier 0/1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillIndexEntry {
    pub name: String,
    pub description: String,
    pub category: Option<String>,
}

/// Learning Loop - improves skills based on experience
pub struct LearningLoop {
    registry: Arc<SkillRegistry>,
    experiences: RwLock<Vec<Experience>>,
}

#[derive(Debug, Clone)]
struct Experience {
    skill_name: String,
    task: String,
    success: bool,
    feedback: String,
    timestamp: chrono::DateTime<chrono::Utc>,
}

impl LearningLoop {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        Self {
            registry,
            experiences: RwLock::new(Vec::new()),
        }
    }

    /// Record an experience with a skill
    pub fn record(&self, skill_name: &str, task: &str, success: bool, feedback: &str) {
        let exp = Experience {
            skill_name: skill_name.to_string(),
            task: task.to_string(),
            success,
            feedback: feedback.to_string(),
            timestamp: chrono::Utc::now(),
        };
        self.experiences.write().push(exp);
    }

    /// Improve skills based on collected experiences
    pub async fn improve(&self) -> Result<Vec<String>> {
        let mut improved = Vec::new();
        let experiences = self.experiences.read().clone();

        // Group by skill
        let mut by_skill: HashMap<String, Vec<&Experience>> = HashMap::new();
        for exp in &experiences {
            by_skill.entry(exp.skill_name.clone())
                .or_default()
                .push(exp);
        }

        for (skill_name, exps) in by_skill {
            // Calculate success rate
            let successes = exps.iter().filter(|e| e.success).count();
            let rate = successes as f32 / exps.len() as f32;

            if rate < 0.7 && exps.len() >= 3 {
                // Skill needs improvement
                let feedback: Vec<&str> = exps.iter()
                    .filter(|e| !e.success)
                    .map(|e| e.feedback.as_str())
                    .collect();

                let improvement = self.suggest_improvement(&skill_name, &feedback).await?;
                
                if let Some(skill) = self.registry.view_skill(&skill_name) {
                    let improved_body = format!("{}\n\n## Improvements\n{}", skill.body, improvement);
                    self.registry.update_skill(&skill_name, &improved_body).await?;
                    improved.push(skill_name);
                }
            }
        }

        Ok(improved)
    }

    async fn suggest_improvement(&self, _skill_name: &str, failures: &[&str]) -> Result<String> {
        // In production, use LLM to analyze failures and suggest improvements
        Ok(format!(
            "Based on {} failures, consider adding:\n- Better error handling\n- More specific instructions\n- Edge case coverage",
            failures.len()
        ))
    }
}